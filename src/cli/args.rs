use crate::address::{HttpAddress, IprotoAddress};
use crate::config::{ByteSize, DEFAULT_USERNAME};
use crate::info::PICODATA_VERSION;
use crate::instance::InstanceName;
use crate::util::Uppercase;
use clap::Parser;
use std::borrow::Cow;
use std::ffi::{CStr, CString};
use std::path::PathBuf;
use tarantool::auth::AuthMethod;
use tarantool::log::SayLevel;
use tarantool::tlua;

#[derive(Debug, Parser)]
#[clap(name = "picodata", version = PICODATA_VERSION)]
pub enum Picodata {
    Run(Box<Run>),
    #[clap(hide = true)]
    Tarantool(Tarantool),
    Expel(Expel),
    Test(Test),
    Connect(Connect),
    Admin(Admin),
    #[clap(subcommand)]
    Config(Config),
}

pub const CONFIG_PARAMETERS_ENV: &'static str = "PICODATA_CONFIG_PARAMETERS";

////////////////////////////////////////////////////////////////////////////////
// Run
////////////////////////////////////////////////////////////////////////////////

#[derive(Debug, Parser, PartialEq)]
#[clap(about = "Run the picodata instance")]
pub struct Run {
    #[clap(long, value_name = "NAME", env = "PICODATA_CLUSTER_NAME")]
    /// Name of the cluster. The instance will refuse
    /// to join a cluster with a different name.
    ///
    /// By default this will be "demo".
    pub cluster_name: Option<String>,

    #[clap(long, value_name = "PATH", env = "PICODATA_DATA_DIR")]
    /// Here the instance persists all of its data.
    ///
    /// By default this is the current working directory (".").
    pub data_dir: Option<PathBuf>,

    #[clap(long, value_name = "PATH", env = "PICODATA_CONFIG_FILE")]
    /// Path to configuration file in yaml format.
    ///
    /// By default "./config.yaml" is used if it exists.
    pub config: Option<PathBuf>,

    #[clap(
        short = 'c',
        long,
        value_name = "PARAMETER=VALUE",
        use_value_delimiter = false
    )]
    /// A list of key-value pairs specifying configuration parameters.
    ///
    /// These will override both parameters provided in the config.yaml file,
    /// the command-line parameters and the environment variables.
    ///
    /// Key is a `.` separated path to a configuration parameter.
    /// The data in the `VALUE` is interpreted as YAML.
    ///
    /// For example: `-c instance.log.level=verbose`
    ///
    /// Can also be provided via PICODATA_CONFIG_PARAMETERS environment variable.
    pub config_parameter: Vec<String>,

    #[clap(long, value_name = "NAME", env = "PICODATA_INSTANCE_NAME")]
    /// Name of the instance.
    /// If not defined, it'll be generated automatically.
    pub instance_name: Option<String>,

    #[clap(
        long = "advertise",
        value_name = "[HOST][:PORT]",
        env = "PICODATA_ADVERTISE"
    )]
    /// Public network address of the instance. It is announced to the
    /// cluster during the instance start. Later it's used by other
    /// instances for connecting to this one.
    ///
    /// Defaults to `--listen` value which is enough in most cases. But,
    /// for example, in case of `--listen 0.0.0.0` it should be
    /// specified explicitly:
    ///
    /// picodata run --listen 0.0.0.0:3301 --advertise 192.168.0.1:3301
    pub advertise_address: Option<IprotoAddress>,

    #[clap(
        short = 'l',
        long = "listen",
        value_name = "[HOST][:PORT]",
        env = "PICODATA_LISTEN"
    )]
    /// Instance network address.
    ///
    /// By default "127.0.0.1:3301" is used.
    pub listen: Option<IprotoAddress>,

    /// Pgproto server address.
    #[clap(long, value_name = "[HOST][:PORT]", env = "PICODATA_PG_LISTEN")]
    pub pg_listen: Option<IprotoAddress>,

    #[clap(
        long = "peer",
        value_name = "[HOST][:PORT]",
        require_value_delimiter = true,
        use_value_delimiter = true,
        env = "PICODATA_PEER"
    )]
    /// A list of network addresses of other instances.
    /// Used during cluster initialization
    /// and joining an instance to an existing cluster.
    ///
    /// Defaults to `--advertise` value which results in creating a new
    /// cluster
    pub peers: Vec<IprotoAddress>,

    #[clap(
        long = "failure-domain",
        value_name = "KEY=VALUE",
        require_value_delimiter = true,
        use_value_delimiter = true,
        parse(try_from_str = try_parse_kv_uppercase),
        env = "PICODATA_FAILURE_DOMAIN"
    )]
    /// Comma-separated list describing physical location of the server.
    /// Each domain is a key-value pair.
    /// Picodata will avoid putting two instances into the same
    /// replicaset if at least one key of their failure domains has the
    /// same value. Instead, new replicasets will be created.
    /// Replicasets will be populated with instances from different
    /// failure domains until the desired replication factor is reached.
    pub failure_domain: Vec<(Uppercase, Uppercase)>,

    #[clap(long, value_name = "NAME", env = "PICODATA_REPLICASET_NAME")]
    /// Name of the replicaset.
    /// Used during cluster initialization
    /// and joining an instance to an existing cluster.
    ///
    /// If not specified, a replicaset will be automatically chosen based on the
    /// failure domain settings.
    pub replicaset_name: Option<String>,

    #[clap(long, arg_enum, env = "PICODATA_LOG_LEVEL")]
    /// Log level.
    ///
    /// By default "info" is used.
    pub log_level: Option<LogLevel>,

    #[clap(long, env = "PICODATA_INIT_REPLICATION_FACTOR", group = "init_cfg")]
    /// Total number of replicas (copies of data) for each replicaset.
    ///
    /// By default 1 is used.
    pub init_replication_factor: Option<u8>,

    #[clap(long, value_name = "PATH", env = "PICODATA_SCRIPT")]
    /// A path to a lua script that will be executed at postjoin stage.
    /// At the moment the script is executed, the local storage is
    /// already initialized and HTTP server is running (if specified).
    /// But the raft node is uninitialized yet.
    pub script: Option<PathBuf>,

    #[clap(long, value_name = "[HOST][:PORT]", env = "PICODATA_HTTP_LISTEN")]
    /// HTTP server address.
    pub http_listen: Option<HttpAddress>,

    #[clap(short = 'i', long = "interactive", env = "PICODATA_INTERACTIVE_MODE")]
    /// Enable interactive console. Deprecated in 24.1.
    pub interactive_mode: bool,

    #[clap(long, value_name = "PATH", env = "PICODATA_ADMIN_SOCK")]
    /// Unix socket for the interactive console to connect using
    /// `picodata admin`. Unlike connecting via `picodata connect`
    /// console communication occurs in plain text
    /// and always operates under the admin account.
    ///
    /// By default the "admin.sock" in the data directory is used.
    // TODO: rename to admin_socket
    pub admin_sock: Option<PathBuf>,

    #[clap(long, value_name = "PATH", env = "PICODATA_PLUGIN_DIR")]
    /// Path to directory with plugin files
    pub plugin_dir: Option<PathBuf>,

    #[clap(long = "tier", value_name = "TIER", env = "PICODATA_INSTANCE_TIER")]
    /// Name of the tier to which the instance will belong.
    /// Used during cluster initialization
    /// and joining an instance to an existing cluster.
    ///
    /// By default "default" is used.
    pub tier: Option<String>,

    /// Filepath to configuration file in yaml format.
    #[clap(
        hide = true,
        long = "init-cfg",
        value_name = "PATH",
        env = "PICODATA_INIT_CFG",
        group = "init_cfg"
    )]
    pub init_cfg: Option<String>,

    #[clap(long = "audit", value_name = "PATH", env = "PICODATA_AUDIT_LOG")]
    // As it's not always a path the value type is left as `String`.
    /// Configuration for the audit log.
    /// Valid options:
    ///
    /// 1. `file:<file>` or simply `<file>` — write to a file, e.g:
    ///
    ///    picodata run --audit '/tmp/audit.log'
    ///
    /// 2. `pipe:<command>` or `| <command>` — redirect to a subprocess, e.g:
    ///
    ///    picodata run --audit '| /bin/capture-from-stdin'
    ///
    /// 3. `syslog:` — write to the syslog, e.g:
    ///
    ///    picodata run --audit 'syslog:'
    ///
    pub audit: Option<String>,

    #[clap(long = "shredding", env = "PICODATA_SHREDDING")]
    /// Shred (not only delete) .xlog and .snap files on rotation
    /// for the security reasons.
    pub shredding: bool,

    #[clap(long = "log", value_name = "PATH", env = "PICODATA_LOG")]
    // As it's not always a path the value type is left as `String`.
    /// Configuration for the picodata diagnostic log.
    /// Valid options:
    ///
    /// 1. `file:<file>` or simply `<file>` — write to a file, e.g.:
    ///
    ///    picodata run --log '/tmp/picodata.log'
    ///
    /// 2. `pipe:<command>` or `| <command>` — redirect to a subprocess, e.g:
    ///
    ///    picodata run --log '| /dev/capture-from-stdin'
    ///
    /// 3. `syslog:` — write to the syslog, e.g:
    ///
    ///    picodata run --log 'syslog:'
    ///
    /// By default, the diagnostic log is output to stderr.
    pub log: Option<String>,

    #[clap(long = "memtx-memory", env = "PICODATA_MEMTX_MEMORY")]
    /// The amount of memory in bytes to allocate for the database engine.
    ///
    /// By default, 64 MiB is used.
    pub memtx_memory: Option<ByteSize>,

    #[clap(
        long = "service-password-file",
        value_name = "PATH",
        env = "PICODATA_SERVICE_PASSWORD_FILE"
    )]
    /// Path to a plain-text file with a password for the system user "pico_service".
    /// This password will be used for internal communication among instances of
    /// picodata, so it must be the same on all instances.
    pub service_password_file: Option<PathBuf>,
}

// Copy enum because clap:ArgEnum can't be derived for the foreign SayLevel.
tarantool::define_str_enum! {
    #[derive(clap::ArgEnum)]
    #[clap(rename_all = "lower")]
    pub enum LogLevel {
        Fatal = "fatal",
        System = "system",
        Error = "error",
        Crit = "crit",
        Warn = "warn",
        Info = "info",
        Verbose = "verbose",
        Debug = "debug",
    }
}

impl From<LogLevel> for SayLevel {
    fn from(l: LogLevel) -> SayLevel {
        match l {
            LogLevel::Fatal => SayLevel::Fatal,
            LogLevel::System => SayLevel::System,
            LogLevel::Error => SayLevel::Error,
            LogLevel::Crit => SayLevel::Crit,
            LogLevel::Warn => SayLevel::Warn,
            LogLevel::Info => SayLevel::Info,
            LogLevel::Verbose => SayLevel::Verbose,
            LogLevel::Debug => SayLevel::Debug,
        }
    }
}

impl Run {
    /// Get the arguments that will be passed to `tarantool_main`
    pub fn tt_args(&self) -> Result<Vec<CString>, String> {
        let mut args = vec![
            current_exe()?,
            CString::new(r"-e").unwrap(),
            CString::new(r#" "#).unwrap(),
        ];

        if self.interactive_mode {
            args.push(CString::new("-i").unwrap());
        }

        Ok(args)
    }
}

////////////////////////////////////////////////////////////////////////////////
// Tarantool
////////////////////////////////////////////////////////////////////////////////

#[derive(Debug, Parser, tlua::Push)]
#[clap(about = "Run tarantool")]
pub struct Tarantool {
    #[clap(raw = true, parse(try_from_str = CString::new))]
    pub args: Vec<CString>,
}

impl Tarantool {
    /// Get the arguments that will be passed to `tarantool_main`
    pub fn tt_args(&self) -> Result<Vec<Cow<CStr>>, String> {
        Ok(std::iter::once(current_exe()?.into())
            .chain(self.args.iter().map(AsRef::as_ref).map(Cow::from))
            .collect())
    }
}

////////////////////////////////////////////////////////////////////////////////
/// Expel
////////////////////////////////////////////////////////////////////////////////

#[derive(Debug, Parser, tlua::Push)]
#[clap(about = "Expel node from cluster")]
pub struct Expel {
    #[clap(
        long,
        value_name = "NAME",
        env = "PICODATA_CLUSTER_NAME",
        default_value = "demo"
    )]
    /// Name of the cluster from instance should be expelled.
    pub cluster_name: String,

    #[clap(value_name = "INSTANCE_NAME")]
    /// Name of the instance to expel.
    pub instance_name: InstanceName,

    #[clap(
        long = "peer",
        value_name = "[HOST][:PORT]",
        env = "PICODATA_PEER",
        default_value = "127.0.0.1:3301"
    )]
    /// Address of any picodata instance of the given cluster.
    pub peer_address: IprotoAddress,

    #[clap(long, env = "PICODATA_PASSWORD_FILE")]
    /// Path to a plain-text file with the `admin` password.
    /// If this option isn't provided, the password is prompted from the terminal.
    pub password_file: Option<String>,

    #[clap(
        short = 'a',
        long = "auth-type",
        value_name = "METHOD",
        default_value = AuthMethod::Md5.as_str(),
    )]
    /// The preferred authentication method.
    pub auth_method: AuthMethod,

    #[clap(
        short = 't',
        long = "timeout",
        value_name = "TIMEOUT",
        default_value = "5",
        env = "PICODATA_CONNECT_TIMEOUT"
    )]
    /// Connection timeout in seconds.
    pub timeout: u64,
}

impl Expel {
    // Get the arguments that will be passed to `tarantool_main`
    pub fn tt_args(&self) -> Result<Vec<CString>, String> {
        Ok(vec![current_exe()?])
    }
}

////////////////////////////////////////////////////////////////////////////////
// Test
////////////////////////////////////////////////////////////////////////////////

#[derive(Debug, Parser, tlua::Push)]
#[clap(about = "Run picodata integration tests")]
pub struct Test {
    #[clap(env = "PICODATA_TEST_FILTER")]
    /// Only run tests matching the filter.
    pub filter: Option<String>,

    #[clap(long = "nocapture", env = "PICODATA_TEST_NOCAPTURE")]
    /// Do not capture test output.
    pub nocapture: bool,
}

impl Test {
    pub fn tt_args(&self) -> Result<Vec<CString>, String> {
        Ok(vec![current_exe()?])
    }
}

////////////////////////////////////////////////////////////////////////////////
// fns
////////////////////////////////////////////////////////////////////////////////

fn current_exe() -> Result<CString, String> {
    CString::new(
        std::env::current_exe()
            .map_err(|e| format!("Failed getting current executable path: {e}"))?
            .display()
            .to_string(),
    )
    .map_err(|e| format!("Current executable path contains nul bytes: {e}"))
}

/// Parses a '=' sepparated string of key and value and converts both to
/// uppercase.
fn try_parse_kv_uppercase(s: &str) -> Result<(Uppercase, Uppercase), String> {
    let (key, value) = s
        .split_once('=')
        .ok_or_else(|| format!("invalid KEY=value: no `=` found in `{}`", s))?;
    Ok((key.into(), value.into()))
}

////////////////////////////////////////////////////////////////////////////////
// Connect
////////////////////////////////////////////////////////////////////////////////

#[derive(Debug, Parser)]
#[clap(about = "Connect to the Distributed SQL console")]
#[clap(after_help = "SPECIAL COMMANDS:
    \\e            Open the editor specified by the EDITOR environment variable
    \\help         Show this screen

HOTKEYS:
    Enter         Submit the request
    Alt  + Enter  Insert a newline character
    Ctrl + C      Discard current input
    Ctrl + D      Quit interactive console
")]
pub struct Connect {
    #[clap(
        short = 'u',
        long = "user",
        value_name = "USER",
        default_value = DEFAULT_USERNAME,
        env = "PICODATA_USER"
    )]
    /// The username to connect with. Ignored if provided in `ADDRESS`.
    pub user: String,

    #[clap(
        short = 'a',
        long = "auth-type",
        value_name = "METHOD",
        default_value = AuthMethod::Md5.as_str(),
    )]
    /// The preferred authentication method.
    pub auth_method: AuthMethod,

    #[clap(value_name = "ADDRESS")]
    /// Picodata instance address to connect. Format:
    /// `[user@][host][:port]`.
    pub address: IprotoAddress,

    #[clap(long, env = "PICODATA_PASSWORD_FILE")]
    /// Path to a plain-text file with a password.
    /// If this option isn't provided, the password is prompted from the terminal.
    pub password_file: Option<String>,

    #[clap(
        short = 't',
        long = "timeout",
        value_name = "TIMEOUT",
        default_value = "5",
        env = "PICODATA_CONNECT_TIMEOUT"
    )]
    /// Connection timeout in seconds.
    pub timeout: u64,
}

impl Connect {
    /// Get the arguments that will be passed to `tarantool_main`
    pub fn tt_args(&self) -> Result<Vec<CString>, String> {
        Ok(vec![current_exe()?])
    }
}

////////////////////////////////////////////////////////////////////////////////
// Admin
////////////////////////////////////////////////////////////////////////////////

#[derive(Debug, Parser)]
#[clap(about = "Connect to the Admin console of a Picodata instance")]
#[clap(after_help = "SPECIAL COMMANDS:
    \\e            Open the editor specified by the EDITOR environment variable
    \\help         Show this screen
    \\sql          Switch console language to SQL (default)
    \\lua          Switch console language to Lua (deprecated)

HOTKEYS:
    Enter         Submit the request
    Alt  + Enter  Insert a newline character
    Ctrl + C      Discard current input
    Ctrl + D      Quit interactive console
")]
pub struct Admin {
    #[clap(value_name = "PATH")]
    /// Unix socket path to connect.
    pub socket_path: String,
}

impl Admin {
    /// Get the arguments that will be passed to `tarantool_main`
    pub fn tt_args(&self) -> Result<Vec<CString>, String> {
        Ok(vec![current_exe()?])
    }
}

////////////////////////////////////////////////////////////////////////////////
// Config
////////////////////////////////////////////////////////////////////////////////

#[derive(Debug, clap::Subcommand)]
#[clap(about = "Subcommands related to working with the configuration file")]
pub enum Config {
    /// Generate a picodata configuration file with default values.
    Default(ConfigDefault),
}

////////////////////////////////////////////////////////////////////////////////
// ConfigDefault
////////////////////////////////////////////////////////////////////////////////

#[derive(Debug, Parser)]
pub struct ConfigDefault {
    #[clap(short = 'o', long = "output-file", value_name = "FILENAME")]
    /// File name for the generated configuration to be written to.
    /// If this option is omitted or the value of "-" is specified,
    /// the file contents are written to the standard output.
    pub output_file: Option<String>,
}
