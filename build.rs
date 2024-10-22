use git_version::git_version;
use std::collections::HashMap;
use std::collections::HashSet;
use std::io::Write;
use std::os::unix::io::{AsRawFd, FromRawFd};
use std::panic::Location;
use std::path::Path;
use std::process::Command;
use std::process::Stdio;

// See also: https://doc.rust-lang.org/cargo/reference/build-scripts.html
fn main() {
    do_compile_time_checks();

    let jobserver = unsafe { jobserver::Client::from_env() };
    let jobserver = jobserver.as_ref();

    // The file structure roughly looks as follows:
    // .
    // ├── build.rs                         // you are here
    // ├── src/
    // ├── tarantool-sys
    // │   ├── CMakeLists.txt // used for dynamic build
    // │   └── static-build
    // │       └── CMakeLists.txt // configures above CMakeLists.txt for static build
    // ├── webui
    // └── <target-dir>/<build-type>/build  // <- build_root
    //     ├── picodata-<smth>/out          // <- std::env::var("OUT_DIR")
    //     ├── webui
    //     ├── tarantool-http
    //     └── tarantool-sys/{static,dynamic}
    //         ├── ncurses-prefix
    //         ├── openssl-prefix
    //         ├── readline-prefix
    //         └── tarantool-prefix
    //
    let out_dir = std::env::var("OUT_DIR").unwrap();
    dbg!(&out_dir); // "<target-dir>/<build-type>/build/picodata-<smth>/out"

    // Running `cargo build` and `cargo clippy` produces 2 different
    // `out_dir` paths. To avoid unnecessary rebuilds we use a different
    // build root for foreign deps (tarantool-sys, tarantool-http, webui)
    let build_root = Path::new(&out_dir).ancestors().nth(2).unwrap();
    dbg!(&build_root); // "<target-dir>/<build-type>/build"

    // See also:
    // https://doc.rust-lang.org/cargo/reference/environment-variables.html#environment-variables-cargo-sets-for-build-scripts
    if let Some(ref makeflags) = std::env::var_os("CARGO_MAKEFLAGS") {
        std::env::set_var("MAKEFLAGS", makeflags);
    }

    #[allow(clippy::print_literal)]
    for (var, value) in std::env::vars() {
        println!("[{}:{}] {var}={value}", file!(), line!());
    }

    println!(
        "cargo:rustc-env=GIT_DESCRIBE={}",
        git_version!(
            fallback = std::env::var("GIT_DESCRIBE")
                .expect("Failed to get version from git and GIT_DESCRIBE env")
        )
    );

    // This variable controls the type of the build for whole project.
    // By default static linking is used (see tarantool-sys/static-build)
    // If feature dynamic_build is set we build tarantool using root cmake project
    // For details on how this passed to build.rs see:
    // https://doc.rust-lang.org/cargo/reference/environment-variables.html#environment-variables-cargo-sets-for-build-scripts
    let use_static_build = std::env::var("CARGO_FEATURE_DYNAMIC_BUILD").is_err();

    generate_export_stubs(&out_dir);
    build_tarantool(jobserver, build_root, use_static_build);
    build_http(jobserver, build_root, use_static_build);
    #[cfg(feature = "webui")]
    build_webui(build_root, Path::new("webui"));
}

fn do_compile_time_checks() {
    let definitions_filename = "src/plugin/ffi.rs";
    println!("cargo:rerun-if-changed={definitions_filename}");
    let declarations_filename = "picoplugin/src/internal/ffi.rs";
    println!("cargo:rerun-if-changed={declarations_filename}");

    if std::env::var("SKIP_FFI_CHECK").is_ok() {
        // Must exist only after the `rerun-if-changed` directives
        return;
    }

    let definitions_source = std::fs::read_to_string(definitions_filename).unwrap();
    let Ok(definitions_ast) = syn::parse_file(&definitions_source) else {
        // Parse errors will be reported by the compiler, just bail out ASAP
        return;
    };

    let declarations_source = std::fs::read_to_string(declarations_filename).unwrap();
    let Ok(declarations_ast) = syn::parse_file(&declarations_source) else {
        // Parse errors will be reported by the compiler, just bail out ASAP
        return;
    };

    let mut definitions = HashMap::new();
    for item in definitions_ast.items {
        let syn::Item::Fn(f) = item else {
            continue;
        };
        let Some(abi) = &f.sig.abi else {
            continue;
        };
        let Some(abi_name) = &abi.name else {
            continue;
        };
        if abi_name.value() != "C" {
            continue;
        }

        let name = f.sig.ident.to_string();
        definitions.insert(name, f);
    }

    let mut declarations = HashMap::new();
    for item in declarations_ast.items {
        let syn::Item::ForeignMod(m) = item else {
            continue;
        };
        for foreign_item in m.items {
            let syn::ForeignItem::Fn(f) = foreign_item else {
                continue;
            };
            let name = f.sig.ident.to_string();
            declarations.insert(name, f);
        }
    }

    for (name, decl) in &declarations {
        let Some(def) = definitions.get(name) else {
            let line = decl.sig.ident.span().start().line;
            println!("cargo:warning={declarations_filename}:{line}: found a declaration for `fn {name}` which is not defined in file {definitions_filename}");
            std::process::exit(1);
        };

        // Compare ignoring ABI because only definition specifies it
        let mut def_sig_no_abi = def.sig.clone();
        def_sig_no_abi.abi.take();

        if def_sig_no_abi != decl.sig {
            let def_sig = &def.sig;
            let def_line = def.sig.ident.span().start().line;
            let def_sig = quote::quote! { #def_sig }.to_string();

            let decl_sig = &decl.sig;
            let decl_line = decl.sig.ident.span().start().line;
            let decl_sig = quote::quote! { #decl_sig }.to_string();
            println!("cargo:warning=Signature mismatch for `fn {name}`");
            println!(
                "
--> {definitions_filename}:{def_line}
|
| {def_sig}

--> {declarations_filename}:{decl_line}
|
| {decl_sig}
    "
            );
            std::process::exit(1);
        }
    }
}

fn generate_export_stubs(out_dir: &str) {
    let mut symbols = HashSet::with_capacity(1024);
    let exports = std::fs::read_to_string("tarantool-sys/extra/exports").unwrap();
    read_symbols_into(&exports, &mut symbols);

    let exports = std::fs::read_to_string("tarantool-sys/extra/exports_libcurl").unwrap();
    read_symbols_into(&exports, &mut symbols);

    let mut code = Vec::with_capacity(2048);
    writeln!(code, "pub fn export_symbols() {{").unwrap();
    writeln!(code, "    extern \"C\" {{").unwrap();
    for symbol in &symbols {
        writeln!(code, "        static {symbol}: *const ();").unwrap();
    }
    writeln!(code, "    }}").unwrap();
    // TODO: use std::hint::black_box, when we move to rust 1.66
    writeln!(code, "    fn black_box(_: *const ()) {{}}").unwrap();
    writeln!(code, "    unsafe {{").unwrap();
    for symbol in &symbols {
        writeln!(code, "        black_box({symbol});").unwrap();
    }
    writeln!(code, "    }}").unwrap();
    writeln!(code, "}}").unwrap();

    let gen_path = std::path::Path::new(out_dir).join("export_symbols.rs");
    std::fs::write(gen_path, code).unwrap();

    fn read_symbols_into<'a>(file_contents: &'a str, symbols: &mut HashSet<&'a str>) {
        for line in file_contents.lines() {
            let line = line.trim();
            if line.starts_with('#') {
                continue;
            }
            if line.is_empty() {
                continue;
            }
            symbols.insert(line);
        }
    }
}

#[cfg(feature = "webui")]
fn rerun_if_webui_changed(webui_dir: &Path) {
    use std::fs;

    let source_dir = std::env::current_dir().unwrap().join(webui_dir);
    // Do not rerun for generated files changes
    let ignored_files = ["node_modules", ".husky"];
    for entry in fs::read_dir(source_dir)
        .expect("failed to scan webui dir")
        .flatten()
    {
        if !ignored_files.contains(&entry.file_name().to_str().unwrap()) {
            println!(
                "cargo:rerun-if-changed={}/{}",
                webui_dir.display(),
                entry.file_name().to_string_lossy()
            );
        }
    }
}

#[cfg(feature = "webui")]
fn build_webui(build_root: &Path, webui_dir: &Path) {
    // make sure to do it first so cargo is notified even if we return early
    rerun_if_webui_changed(webui_dir);
    println!("cargo:rerun-if-env-changed=WEBUI_BUNDLE");

    if std::env::var("WEBUI_BUNDLE").is_ok() {
        println!("building webui_bundle skipped");
        return;
    }

    println!("building webui_bundle ...");
    let source_dir = std::env::current_dir().unwrap().join(webui_dir);
    let out_dir = build_root.join(webui_dir);
    let out_dir_str = out_dir.display().to_string();

    let webui_bundle = out_dir.join("bundle.json");
    println!("cargo:rustc-env=WEBUI_BUNDLE={}", webui_bundle.display());

    Command::new("yarn")
        .arg("install")
        .arg("--prefer-offline")
        .arg("--frozen-lockfile")
        .arg("--no-progress")
        .arg("--non-interactive")
        .current_dir(&source_dir)
        .run();
    Command::new("yarn")
        .arg("vite")
        .arg("build")
        .args(["--outDir", &out_dir_str])
        .arg("--emptyOutDir")
        .current_dir(&source_dir)
        .run();
}

const TARANTOOL_SYS_STATIC: &str = "tarantool-sys/static";
const TARANTOOL_SYS_DYNAMIC: &str = "tarantool-sys/dynamic";

fn build_http(jsc: Option<&jobserver::Client>, build_root: &Path, use_static_build: bool) {
    println!("cargo:rerun-if-changed=http/http");

    let build_dir = build_root.join("tarantool-http");
    let build_dir_str = build_dir.display().to_string();

    let tarantool_dir = if use_static_build {
        build_root
            .join(TARANTOOL_SYS_STATIC)
            .join("tarantool-prefix")
    } else {
        build_root.join(TARANTOOL_SYS_DYNAMIC)
    };

    Command::new("cmake")
        .args(["-S", "http"])
        .args(["-B", &build_dir_str])
        .arg(format!("-DTARANTOOL_DIR={}", tarantool_dir.display()))
        .run();

    let mut cmd = Command::new("cmake");
    cmd.args(["--build", &build_dir_str]);
    if let Some(jsc) = jsc {
        jsc.configure(&mut cmd);
    }
    cmd.run();

    Command::new("ar")
        .arg("-rcs")
        .arg(build_dir.join("libhttpd.a"))
        .arg(build_dir.join("http/CMakeFiles/httpd.dir/lib.c.o"))
        .run();

    rustc::link_search(build_dir_str);
}

fn build_tarantool(jsc: Option<&jobserver::Client>, build_root: &Path, use_static_build: bool) {
    println!("cargo:rerun-if-changed=tarantool-sys");

    let tarantool_sys = if use_static_build {
        build_root.join(TARANTOOL_SYS_STATIC)
    } else {
        build_root.join(TARANTOOL_SYS_DYNAMIC)
    };

    let tarantool_build = if use_static_build {
        tarantool_sys.join("tarantool-prefix/src/tarantool-build")
    } else {
        tarantool_sys.clone()
    };

    if !tarantool_build.exists() {
        // Build from scratch
        let mut configure_cmd = Command::new("cmake");
        configure_cmd.arg("-B").arg(&tarantool_sys);

        let common_args = [
            "-DCMAKE_BUILD_TYPE=RelWithDebInfo",
            "-DBUILD_TESTING=FALSE",
            "-DBUILD_DOC=FALSE",
        ];

        if use_static_build {
            // In pgproto, we use openssl crate that links to openssl from the system, so we want
            // tarantool to link to the same library. Otherwise there will be conflicts between
            // different library versions.
            //
            // In theory, the crate can use openssl from tarantool. There is a variable
            // OPENSSL_DIR that points to the directory with the library. But to use it we need
            // to build the crate after tarantool, while the order is not determined.
            // Thus, in practice, openssl crate builds before tarantool and throws an error that it
            // can't find the library because it wasn't built yet.
            let args = [&common_args[..], &["-DOPENSSL_USE_STATIC_LIBS=FALSE"]].concat();

            // static build is a separate project that uses CMAKE_TARANTOOL_ARGS
            // to forward parameters to tarantool cmake project
            configure_cmd
                .args(["-S", "tarantool-sys/static-build"])
                .arg(format!("-DCMAKE_TARANTOOL_ARGS={}", &args.join(";")))
        } else {
            // for dynamic build we do not use most of the bundled dependencies
            configure_cmd
                .args(["-S", "tarantool-sys"])
                .args(common_args)
                .args([
                    "-DENABLE_BUNDLED_LDAP=OFF",
                    "-DENABLE_BUNDLED_LIBCURL=OFF",
                    "-DENABLE_BUNDLED_LIBUNWIND=OFF",
                    "-DENABLE_BUNDLED_LIBYAML=OFF",
                    "-DENABLE_BUNDLED_OPENSSL=OFF",
                    "-DENABLE_BUNDLED_ZSTD=OFF",
                ])
                // for dynamic build we'll also need to install the project, so configure the prefix
                .arg(format!(
                    "-DCMAKE_INSTALL_PREFIX={}",
                    tarantool_sys.display(),
                ))
        }
        .run();
    }

    let mut build_cmd = Command::new("cmake");
    build_cmd.arg("--build").arg(&tarantool_sys);

    if !use_static_build {
        build_cmd.args(["--", "install"]);
    }

    if let Some(jsc) = jsc {
        jsc.configure(&mut build_cmd);
    }
    build_cmd.run();

    let tarantool_sys = tarantool_sys.display();
    let tarantool_build = tarantool_build.display();

    // Don't build a shared object in case it's the default for the compiler
    rustc::link_arg("-no-pie");

    for l in [
        "core",
        "small",
        "msgpuck",
        "vclock",
        "bit",
        "swim",
        "uri",
        "json",
        "http_parser",
        "mpstream",
        "raft",
        "csv",
        "bitset",
        "coll",
        "fakesys",
        "salad",
        "tzcode",
    ] {
        rustc::link_search(format!("{tarantool_build}/src/lib/{l}"));
        rustc::link_lib_static(l);
    }

    rustc::link_search(format!("{tarantool_build}/src/lib/crypto"));
    rustc::link_lib_static("tcrypto");

    rustc::link_search(format!("{tarantool_build}"));
    rustc::link_search(format!("{tarantool_build}/src"));
    rustc::link_search(format!("{tarantool_build}/src/box"));
    rustc::link_search(format!("{tarantool_build}/third_party/c-dt/build"));
    rustc::link_search(format!("{tarantool_build}/third_party/luajit/src"));

    if use_static_build {
        rustc::link_search(format!("{tarantool_build}/build/libyaml/lib"));
        rustc::link_search(format!("{tarantool_build}/build/nghttp2/dest/lib"));
    }

    rustc::link_lib_static("tarantool");

    if use_static_build {
        rustc::link_lib_static("ev")
    } else {
        rustc::link_lib_dynamic("ev");
    }

    rustc::link_lib_static("coro");
    rustc::link_lib_static("cdt");
    rustc::link_lib_static("server");
    rustc::link_lib_static("misc");
    rustc::link_lib_static("decNumber");
    rustc::link_lib_static("eio");
    rustc::link_lib_static("box");
    rustc::link_lib_static("tuple");
    rustc::link_lib_static("xrow");
    rustc::link_lib_static("box_error");
    rustc::link_lib_static("xlog");
    rustc::link_lib_static("crc32");
    rustc::link_lib_static("stat");
    rustc::link_lib_static("shutdown");
    rustc::link_lib_static("swim_udp");
    rustc::link_lib_static("swim_ev");
    rustc::link_lib_static("symbols");
    rustc::link_lib_static("cpu_feature");
    rustc::link_lib_static("luajit");
    rustc::link_lib_static("xxhash");

    if use_static_build {
        rustc::link_lib_static("nghttp2");
        rustc::link_lib_static("zstd");
        rustc::link_lib_static("yaml_static");
    } else {
        rustc::link_lib_dynamic("yaml");
        rustc::link_lib_dynamic("zstd");
    }

    // Add LDAP authentication support libraries.
    if use_static_build {
        rustc::link_search(format!("{tarantool_build}/bundled-ldap-prefix/lib"));
        rustc::link_lib_static_no_whole_archive("ldap");
        rustc::link_lib_static_no_whole_archive("lber");
        rustc::link_search(format!("{tarantool_build}/bundled-sasl-prefix/lib"));
        rustc::link_lib_static_no_whole_archive("sasl2");
    } else {
        rustc::link_lib_dynamic("sasl2");
        rustc::link_lib_dynamic("ldap");
    }

    if cfg!(target_os = "macos") {
        // Currently we link against 2 versions of `decNumber` library: one
        // comes with tarantool and ther other comes from the `dec` cargo crate.
        // On macos this seems to confuse the linker, which just chooses one of
        // the libraries and complains that it can't find symbols from the other.
        // So we add the second library explicitly via full path to the file.
        rustc::link_arg(format!("{tarantool_build}/libdecNumber.a"));

        // Libunwind is built into the compiler on macos
    } else {
        // These two must be linked as positional arguments, because they define
        // duplicate symbols which is not allowed (by default) when linking with
        // via -l... option
        let arch = std::env::var("CARGO_CFG_TARGET_ARCH").unwrap();
        if use_static_build {
            let lib_dir = format!("{tarantool_build}/third_party/libunwind/src/.libs");
            rustc::link_arg(format!("{lib_dir}/libunwind-{arch}.a"));
            rustc::link_arg(format!("{lib_dir}/libunwind.a"));
        } else {
            rustc::link_lib_dynamic(format!("unwind-{arch}"));
            rustc::link_lib_dynamic("unwind");
        }
    }

    rustc::link_arg("-lc");

    if use_static_build {
        rustc::link_search(format!("{tarantool_sys}/readline-prefix/lib"));
        rustc::link_lib_static("readline");

        rustc::link_search(format!("{tarantool_sys}/icu-prefix/lib"));
        rustc::link_lib_static("icudata");
        rustc::link_lib_static("icui18n");
        rustc::link_lib_static("icuio");
        rustc::link_lib_static("icutu");
        rustc::link_lib_static("icuuc");

        // "z" is linked with curl on cmake stage in case of a dynamic build
        rustc::link_search(format!("{tarantool_sys}/zlib-prefix/lib"));
        rustc::link_lib_static("z");

        rustc::link_search(format!("{tarantool_build}/build/curl/dest/lib"));
        rustc::link_lib_static("curl");

        // c-ares not used with dynamic build because we have curl-openssl-dev
        rustc::link_search(format!("{tarantool_build}/build/ares/dest/lib"));
        rustc::link_lib_dynamic("cares");

        rustc::link_search(format!("{tarantool_sys}/ncurses-prefix/lib"));
        rustc::link_lib_static("tinfo");
    } else {
        if cfg!(target_os = "macos") {
            // On macos icu4c and readline are keg-only, which means they were not
            // symlinked into /usr/local. We should add the search path manually.
            rustc::link_search("/usr/local/opt/icu4c/lib");
            rustc::link_search("/usr/local/opt/readline/lib");
        }

        rustc::link_lib_dynamic("readline");

        rustc::link_lib_dynamic("icudata");
        rustc::link_lib_dynamic("icui18n");
        rustc::link_lib_dynamic("icuio");
        rustc::link_lib_dynamic("icutu");
        rustc::link_lib_dynamic("icuuc");

        rustc::link_lib_dynamic("curl");

        rustc::link_lib_dynamic("ssl");
        rustc::link_lib_dynamic("crypto");
    }

    // Previously in static build mode we used to link both libssl
    // and libcrypto statically. However with the addition of libssl
    // support to pgproto we needed to use Rust ssl bindings which
    // need to link with libssl too. To unify tarantool and rust side
    // of things we decided to always link libssl dynamically. When
    // libssl is linked dynamically there is no need to link libcrypto
    // because it is a dependency of libssl and is already present in
    // libssl.so:
    // > ldd /usr/lib64/libssl.so
    //     libcrypto.so.3 => /lib64/libcrypto.so.3
    //     ...
    //
    // If needed this is the way to link libssl statically:
    // rustc::link_lib_static("ssl");
    // rustc::link_lib_static("crypto");
    rustc::link_lib_dynamic("ssl");

    rustc::link_search(format!("{tarantool_sys}/iconv-prefix/lib"));
    if cfg!(target_os = "macos") {
        // -lc++ instead of -lstdc++ on macos
        rustc::link_lib_dynamic("c++");

        // -lresolv on macos
        rustc::link_lib_dynamic("resolv");
    } else {
        // not supported on macos
        // We use -rdynamic instead of -export-dynamic
        // because on fedora this likely triggers this bug
        // https://gcc.gnu.org/bugzilla/show_bug.cgi?id=47390
        rustc::link_arg("-rdynamic");
        rustc::link_lib_dynamic("stdc++");
    }
}

trait CommandExt {
    fn run(&mut self);
}

impl CommandExt for Command {
    #[track_caller]
    fn run(&mut self) {
        let loc = Location::caller();
        println!("[{}:{}] running [{:?}]", loc.file(), loc.line(), self);

        // Redirect stderr to stdout. This is needed to preserve the order of
        // error messages in the output, because otherwise cargo separates the
        // streams into 2 chunks destroying any hope of understanding when the
        // errors happened.
        let stdout_fd = std::io::stdout().as_raw_fd();
        let stdout_dup_fd = nix::unistd::dup(stdout_fd).expect("what could go wrong?");
        let stdout = unsafe { Stdio::from_raw_fd(stdout_dup_fd) };
        self.stderr(stdout);

        let prog = self.get_program().to_owned().into_string().unwrap();

        match self.status() {
            Ok(status) if status.success() => (),
            Ok(status) => panic!("{} failed: {}", prog, status),
            Err(e) => panic!("failed running `{}`: {}", prog, e),
        }
    }
}

mod rustc {
    pub fn link_search(path: impl AsRef<str>) {
        println!("cargo:rustc-link-search=native={}", path.as_ref());
    }

    pub fn link_lib_static(lib: impl AsRef<str>) {
        // See also:
        // https://doc.rust-lang.org/cargo/reference/build-scripts.html#cargorustc-link-liblib
        // https://doc.rust-lang.org/rustc/command-line-arguments.html#option-l-link-lib
        println!(
            "cargo:rustc-link-lib=static:+whole-archive,-bundle={}",
            lib.as_ref()
        );
    }

    // NB: this is needed less often and thus designed to be opt-out.
    pub fn link_lib_static_no_whole_archive(lib: impl AsRef<str>) {
        println!("cargo:rustc-link-lib=static:-bundle={}", lib.as_ref());
    }

    pub fn link_lib_dynamic(lib: impl AsRef<str>) {
        println!("cargo:rustc-link-lib=dylib={}", lib.as_ref());
    }

    pub fn link_arg(arg: impl AsRef<str>) {
        println!("cargo:rustc-link-arg={}", arg.as_ref());
    }
}
