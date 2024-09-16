use crate::config::BootstrapStrategy;
use crate::config::ElectionMode;
use crate::config::PicodataConfig;
use crate::config_parameter_path;
use crate::instance::Instance;
use crate::introspection::Introspection;
use crate::rpc::join;
use crate::schema::PICO_SERVICE_USER_NAME;
use crate::traft::error::Error;
use ::tarantool::fiber;
use ::tarantool::lua_state;
use ::tarantool::msgpack::ViaMsgpack;
use ::tarantool::tlua::{self, LuaError, LuaFunction, LuaRead, LuaTable, LuaThread, PushGuard};
pub use ::tarantool::trigger::on_shutdown;
use file_shred::*;
use std::collections::HashMap;
use std::ffi::CStr;
use std::os::unix::ffi::OsStrExt;
use std::path::PathBuf;
use tlua::CallError;

#[macro_export]
macro_rules! stringify_last_token {
    ($tail:tt) => { std::stringify!($tail) };
    ($head:tt $($tail:tt)+) => { $crate::stringify_last_token!($($tail)+) };
}

/// Checks that the given function exists and returns it's name suitable for
/// calling it via tarantool rpc.
///
/// The argument can be a full path to the function.
#[macro_export]
macro_rules! proc_name {
    ( $($func_name:tt)+ ) => {{
        use ::tarantool::tuple::FunctionArgs;
        use ::tarantool::tuple::FunctionCtx;
        use libc::c_int;

        const _: unsafe extern "C" fn(FunctionCtx, FunctionArgs) -> c_int = $($func_name)+;
        concat!(".", $crate::stringify_last_token!($($func_name)+))
    }};
}

mod ffi {
    use libc::c_char;
    use libc::c_int;
    use libc::c_void;

    extern "C" {
        pub fn tarantool_version() -> *const c_char;
        pub fn tarantool_package() -> *const c_char;
        pub fn tarantool_main(
            argc: i32,
            argv: *mut *mut c_char,
            cb: Option<extern "C" fn(*mut c_void)>,
            cb_data: *mut c_void,
        ) -> c_int;
    }
}
pub use ffi::tarantool_main as main;

pub fn version() -> &'static str {
    let c_ptr = unsafe { ffi::tarantool_version() };
    let c_str = unsafe { CStr::from_ptr(c_ptr) };
    return c_str.to_str().unwrap();
}

pub fn package() -> &'static str {
    let c_ptr = unsafe { ffi::tarantool_package() };
    let c_str = unsafe { CStr::from_ptr(c_ptr) };
    return c_str.to_str().unwrap();
}

mod tests {
    use super::*;

    #[::tarantool::test]
    fn test_version() {
        let l = lua_state();
        let t: LuaTable<_> = l.eval("return require('tarantool')").unwrap();
        assert_eq!(version(), t.get::<String, _>("version").unwrap());
        assert_eq!(package(), t.get::<String, _>("package").unwrap());
    }
}

/// Tarantool configuration.
/// See <https://www.tarantool.io/en/doc/latest/reference/configuration/#configuration-parameters>
#[derive(Clone, Debug, Default, serde::Serialize, serde::Deserialize, PartialEq)]
pub struct Cfg {
    pub instance_uuid: Option<String>,
    pub replicaset_uuid: Option<String>,

    pub listen: Option<String>,

    pub read_only: bool,
    pub replication: Vec<String>,

    pub bootstrap_strategy: Option<BootstrapStrategy>,

    pub election_mode: ElectionMode,

    pub log: Option<String>,
    pub log_level: Option<u8>,

    pub wal_dir: PathBuf,
    pub memtx_dir: PathBuf,
    pub vinyl_dir: PathBuf,

    #[serde(flatten)]
    pub other_fields: HashMap<String, rmpv::Value>,
}

impl Cfg {
    /// Temporary minimal configuration. After initializing with this
    /// configuration we either will go into discovery phase after which we will
    /// rebootstrap and go to the next phase (either boot or join), or if the
    /// storage is already initialize we will go into the post join phase.
    pub fn for_discovery(config: &PicodataConfig) -> Result<Self, Error> {
        let mut res = Self {
            // These will either be chosen on the next phase or are already
            // chosen and will be restored from the local storage.
            instance_uuid: None,
            replicaset_uuid: None,

            // Listen port will be set a bit later.
            listen: None,

            // On discovery stage the local storage needs to be bootstrapped,
            // but if we're restarting this will be changed to `true`, because
            // we can't be a replication master at that point.
            read_only: false,

            // During discovery phase we either don't set up the replication, or
            // the replicaset is already bootstrapped, so bootstrap strategy is
            // irrelevant.
            bootstrap_strategy: Some(BootstrapStrategy::Auto),

            election_mode: ElectionMode::Off,

            // If this is a restart, replication will be configured by governor
            // before our state changes to Online.
            ..Default::default()
        };

        res.set_core_parameters(config)?;
        Ok(res)
    }

    /// Initial configuration for the cluster bootstrap phase.
    pub fn for_cluster_bootstrap(
        config: &PicodataConfig,
        leader: &Instance,
    ) -> Result<Self, Error> {
        let mut res = Self {
            // At this point uuids must be valid, it will be impossible to
            // change them until the instance is expelled.
            instance_uuid: Some(leader.instance_uuid.clone()),
            replicaset_uuid: Some(leader.replicaset_uuid.clone()),

            // Listen port will be set after the global raft node is initialized.
            listen: None,

            // Must be writable, we're going to initialize the storage.
            read_only: false,

            // During bootstrap the replicaset contains only one instance,
            // so bootstrap strategy is irrelevant.
            bootstrap_strategy: Some(BootstrapStrategy::Auto),

            election_mode: ElectionMode::Off,

            // Replication will be configured by governor when another replica
            // joins.
            ..Default::default()
        };

        res.set_core_parameters(config)?;
        Ok(res)
    }

    /// Initial configuration for the new instance joining to an already
    /// initialized cluster.
    pub fn for_instance_join(
        config: &PicodataConfig,
        resp: &join::Response,
    ) -> Result<Self, Error> {
        let mut replication_cfg = Vec::with_capacity(resp.box_replication.len());
        let password = crate::pico_service::pico_service_password();
        for address in &resp.box_replication {
            replication_cfg.push(format!("{PICO_SERVICE_USER_NAME}:{password}@{address}"))
        }

        let mut res = Self {
            // At this point uuids must be valid, it will be impossible to
            // change them until the instance is expelled.
            instance_uuid: Some(resp.instance.instance_uuid.clone()),
            replicaset_uuid: Some(resp.instance.replicaset_uuid.clone()),

            // Needs to be set, because an applier will attempt to connect to
            // self and will block box.cfg() call until it succeeds.
            listen: Some(config.instance.listen().to_host_port()),

            // If we're joining to an existing replicaset,
            // then we're the follower.
            read_only: replication_cfg.len() > 1,

            // Always contains the current instance.
            replication: replication_cfg,

            // TODO: in tarantool-3.0 there's a new bootstrap_strategy = "config"
            // which allows to specify the bootstrap leader explicitly. This is
            // what we want to use, so we should switch to that when that
            // feature is available in our tarantool fork.
            //
            // When joining we already know the leader, and the "auto" strategy
            // implies connecting to that leader during bootstrap.
            bootstrap_strategy: Some(BootstrapStrategy::Auto),

            election_mode: ElectionMode::Off,

            ..Default::default()
        };

        res.set_core_parameters(config)?;
        Ok(res)
    }

    pub fn set_core_parameters(&mut self, config: &PicodataConfig) -> Result<(), Error> {
        self.log.clone_from(&config.instance.log.destination);
        self.log_level = Some(config.instance.log_level() as _);

        self.wal_dir = config.instance.data_dir();
        self.memtx_dir = config.instance.data_dir();
        self.vinyl_dir = config.instance.data_dir();

        // FIXME: make the loop below work with default values
        self.other_fields
            .insert("memtx_memory".into(), config.instance.memtx_memory().into());

        #[rustfmt::skip]
        const MAPPING: &[(&str, &str)] = &[
            // Other instance.log.* parameters are set explicitly above
            ("checkpoint_count",            config_parameter_path!(instance.memtx.checkpoint_count)),
            ("checkpoint_interval",         config_parameter_path!(instance.memtx.checkpoint_interval)),
            ("log_format",                  config_parameter_path!(instance.log.format)),
            ("vinyl_memory",                config_parameter_path!(instance.vinyl.memory)),
            ("vinyl_cache",                 config_parameter_path!(instance.vinyl.cache)),
            ("net_msg_max",                 config_parameter_path!(instance.iproto.max_concurrent_messages)),
        ];
        for (box_field, picodata_field) in MAPPING {
            let value = config
                .get_field_as_rmpv(picodata_field)
                .map_err(|e| Error::other(format!("internal error: {e}")))?;
            self.other_fields.insert((*box_field).into(), value);
        }

        Ok(())
    }
}

pub fn is_box_configured() -> bool {
    let lua = lua_state();
    let box_: Option<LuaTable<_>> = lua.get("box");
    let Some(box_) = box_ else {
        return false;
    };
    let box_cfg: Option<LuaTable<_>> = box_.get("cfg");
    box_cfg.is_some()
}

#[track_caller]
pub fn set_cfg(cfg: &Cfg) -> Result<(), Error> {
    let lua = lua_state();
    let res = lua.exec_with("return box.cfg(...)", ViaMsgpack(cfg));
    match res {
        Err(CallError::PushError(e)) => {
            crate::tlog!(Error, "failed to push box configuration via msgpack: {e}");
            return Err(Error::other(e));
        }
        Err(CallError::LuaError(e)) => {
            return Err(Error::other(e));
        }
        Ok(()) => {}
    }
    Ok(())
}

#[allow(dead_code)]
pub fn cfg_field<T>(field: &str) -> Option<T>
where
    T: LuaRead<PushGuard<LuaTable<PushGuard<LuaTable<PushGuard<LuaThread>>>>>>,
{
    let l = lua_state();
    let b: LuaTable<_> = l.into_get("box").ok()?;
    let cfg: LuaTable<_> = b.into_get("cfg").ok()?;
    cfg.into_get(field).ok()
}

#[inline]
pub fn set_cfg_field<T>(field: &str, value: T) -> Result<(), tlua::LuaError>
where
    T: tlua::PushOneInto<tlua::LuaState>,
    tlua::Void: From<T::Err>,
{
    set_cfg_fields(((field, value),))
}

pub fn set_cfg_fields<T>(table: T) -> Result<(), tlua::LuaError>
where
    tlua::AsTable<T>: tlua::PushInto<tlua::LuaState>,
{
    use tlua::Call;

    let l = lua_state();
    let b: LuaTable<_> = l.get("box").expect("can't fail under tarantool");
    let cfg: tlua::Callable<_> = b.get("cfg").expect("can't fail under tarantool");
    cfg.call_with(tlua::AsTable(table)).map_err(|e| match e {
        CallError::PushError(_) => unreachable!("cannot fail during push"),
        CallError::LuaError(e) => e,
    })
}

#[track_caller]
pub fn exec(code: &str) -> Result<(), LuaError> {
    let l = lua_state();
    l.exec(code)
}

#[track_caller]
pub fn eval<T>(code: &str) -> Result<T, LuaError>
where
    T: for<'l> LuaRead<PushGuard<LuaFunction<PushGuard<&'l LuaThread>>>>,
{
    let l = lua_state();
    l.eval(code)
}

/// Analogue of tarantool's `os.exit(code)`. Use this function if tarantool's
/// [`on_shutdown`] triggers must run. If instead you want to skip on_shutdown
/// triggers, use [`std::process::exit`] instead.
///
/// [`on_shutdown`]: ::tarantool::trigger::on_shutdown
pub fn exit(code: i32) -> ! {
    unsafe { tarantool_exit(code) }

    loop {
        fiber::fiber_yield()
    }

    extern "C" {
        fn tarantool_exit(code: i32);
    }
}

extern "C" {
    /// This variable need to replace the implemetation of function
    /// uses by xlog_remove_file() to removes an .xlog and .snap files.
    /// <https://git.picodata.io/picodata/tarantool/-/blob/2.11.2-picodata/src/box/xlog.c#L2145>
    ///
    /// In default implementation:
    /// On success, set the 'existed' flag to true if the file existed and was
    /// actually deleted or to false otherwise and returns 0. On failure, sets
    /// diag and returns -1.
    ///
    /// Note that default function didn't treat ENOENT as error and same behavior
    /// mostly recommended.
    pub static mut xlog_remove_file_impl: extern "C" fn(
        filename: *const std::os::raw::c_char,
        existed: *mut bool,
    ) -> std::os::raw::c_int;
}

pub fn xlog_set_remove_file_impl() {
    unsafe { xlog_remove_file_impl = xlog_remove_cb };
}

extern "C" fn xlog_remove_cb(
    filename: *const std::os::raw::c_char,
    existed: *mut bool,
) -> std::os::raw::c_int {
    const OVERWRITE_COUNT: u32 = 6;
    const RENAME_COUNT: u32 = 4;

    let c_str = unsafe { std::ffi::CStr::from_ptr(filename) };
    let os_str = std::ffi::OsStr::from_bytes(c_str.to_bytes());
    let path: &std::path::Path = os_str.as_ref();

    let filename = path.display();
    crate::tlog!(Info, "shredding started for: {filename}");
    crate::audit!(
        message: "shredding started for {filename}",
        title: "shredding_started",
        severity: Low,
        filename: &filename,
    );

    let path_exists = path.exists();
    unsafe { *existed = path_exists };

    if !path_exists {
        return 0;
    }

    let config = ShredConfig::<std::path::PathBuf>::non_interactive(
        vec![std::path::PathBuf::from(path)],
        Verbosity::Debug,
        crate::error_injection::is_enabled("KEEP_FILES_AFTER_SHREDDING"),
        OVERWRITE_COUNT,
        RENAME_COUNT,
    );

    return match shred(&config) {
        Ok(_) => {
            crate::tlog!(Info, "shredding finished for: {filename}");
            crate::audit!(
                message: "shredding finished for {filename}",
                title: "shredding_finished",
                severity: Low,
                filename: &filename,
            );
            0
        }
        Err(err) => {
            crate::tlog!(Error, "shredding failed due to: {err}");
            crate::audit!(
                message: "shredding failed for {filename}",
                title: "shredding_failed",
                severity: Low,
                error: &err,
                filename: &filename,
            );
            -1
        }
    };
}

extern "C" {
    /// Sets an IPROTO request handler with the provided
    /// context for the given request type.
    pub fn box_iproto_override(
        req_type: u32,
        handler: Option<iproto_handler_t>,
        destroy: Option<iproto_handler_destroy_t>,
        ctx: *mut (),
    ) -> i32;
}

/// Callback for overwritten handlers of IPROTO requests.
/// Sets diagnostic message and returns and error to register it.
pub extern "C" fn iproto_override_cb(
    _header: *const u8,
    _header_end: *const u8,
    _body: *const u8,
    _body_end: *const u8,
    _ctx: *mut (),
) -> iproto_handler_status {
    ::tarantool::set_error!(
        ::tarantool::error::TarantoolErrorCode::Unsupported,
        "picodata does not support this IPROTO request type, it was disabled"
    );
    iproto_handler_status::IPROTO_HANDLER_ERROR
}

/// Return codes for IPROTO request handlers.
#[allow(dead_code)]
#[repr(C)]
pub enum IprotoHandlerStatus {
    IPROTO_HANDLER_OK = 0,
    IPROTO_HANDLER_ERROR = 1,
    IPROTO_HANDLER_FALLBACK = 2,
}

/// Status of handlers of IPROTO requests when
/// request path of handling is overwritten.
type iproto_handler_status = IprotoHandlerStatus;

/// Type of callback for a IPROTO request handler.
type iproto_handler_t = extern "C" fn(
    header: *const u8,
    header_end: *const u8,
    body: *const u8,
    body_end: *const u8,
    ctx: *mut (),
) -> iproto_handler_status;

/// Type of destroy callback for a IPROTO request handler.
type iproto_handler_destroy_t = extern "C" fn(ctx: *mut ());

pub fn rm_tarantool_files(
    data_dir: impl AsRef<std::path::Path>,
) -> Result<(), tarantool::error::Error> {
    let entries = std::fs::read_dir(data_dir)?;
    for entry in entries {
        let path = entry?.path();
        if !path.is_file() {
            continue;
        }

        let Some(ext) = path.extension() else {
            continue;
        };

        if ext != "xlog" && ext != "snap" {
            continue;
        }

        crate::tlog!(Info, "removing file: {}", path.display());
        std::fs::remove_file(path)?;
    }
    Ok(())
}

pub fn box_schema_version() -> u64 {
    mod ffi {
        extern "C" {
            pub fn box_schema_version() -> u64;
        }
    }

    // Safety: always safe
    unsafe { ffi::box_schema_version() }
}
