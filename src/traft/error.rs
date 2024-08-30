use std::fmt::{Debug, Display, Formatter};

use crate::error_code::ErrorCode;
use crate::instance::InstanceId;
use crate::plugin::PluginError;
use crate::traft::{RaftId, RaftTerm};
use tarantool::error::IntoBoxError;
use tarantool::error::{BoxError, TarantoolErrorCode};
use tarantool::fiber::r#async::timeout;
use tarantool::tlua::LuaError;
use thiserror::Error;

#[derive(Debug)]
pub struct Unsupported {
    entity: String,
    help: Option<String>,
}

impl Unsupported {
    pub(crate) fn new(entity: String, help: Option<String>) -> Self {
        Self { entity, help }
    }
}

impl Display for Unsupported {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write!(f, "unsupported action/entity: {}", self.entity)?;
        if let Some(ref help) = self.help {
            write!(f, ", {help}")?;
        }
        Ok(())
    }
}

#[derive(Debug, Error)]
pub enum Error {
    #[error("uninitialized yet")]
    Uninitialized,
    #[error("timeout")]
    Timeout,
    #[error("current instance is expelled from the cluster")]
    Expelled,
    #[error("{0}")]
    Raft(#[from] raft::Error),
    #[error("downcast error: expected {expected:?}, actual: {actual:?}")]
    DowncastError {
        expected: &'static str,
        actual: &'static str,
    },
    /// cluster_id of the joining instance mismatches the cluster_id of the cluster
    #[error("cluster_id mismatch: cluster_id of the instance = {instance_cluster_id:?}, cluster_id of the cluster = {cluster_cluster_id:?}")]
    ClusterIdMismatch {
        instance_cluster_id: String,
        cluster_cluster_id: String,
    },
    /// Instance was requested to configure replication with different replicaset.
    #[error("cannot replicate with different replicaset: expected {instance_rsid:?}, requested {requested_rsid:?}")]
    ReplicasetIdMismatch {
        instance_rsid: String,
        requested_rsid: String,
    },
    // NOTE: this error message is relied on in luamod.lua,
    // don't forget to update it everywhere if you're changing it.
    #[error("operation request from different term {requested}, current term is {current}")]
    TermMismatch {
        requested: RaftTerm,
        current: RaftTerm,
    },
    // NOTE: this error message is relied on in luamod.lua,
    // don't forget to update it everywhere if you're changing it.
    #[error("not a leader")]
    NotALeader,
    #[error("lua error: {0}")]
    Lua(#[from] LuaError),
    #[error("{0}")]
    Tarantool(#[from] ::tarantool::error::Error),
    #[error("instance with {} not found", DisplayIdOfInstance(.0))]
    NoSuchInstance(Result<RaftId, InstanceId>),
    #[error("replicaset with {} \"{id}\" not found", if *.id_is_uuid { "replicaset_uuid" } else { "replicaset_id" })]
    NoSuchReplicaset { id: String, id_is_uuid: bool },
    #[error("address of peer with id {0} not found")]
    AddressUnknownForRaftId(RaftId),
    #[error("address of peer with id \"{0}\" not found")]
    AddressUnknownForInstanceId(InstanceId),
    #[error("address of peer is incorrectly formatted: {0}")]
    AddressParseFailure(String),
    #[error("leader is unknown yet")]
    LeaderUnknown,
    #[error("governor has stopped")]
    GovernorStopped,

    #[error("{0}")]
    Cas(#[from] crate::cas::Error),
    #[error("{0}")]
    Ddl(#[from] crate::schema::DdlError),

    #[error("sbroad: {0}")]
    Sbroad(#[from] sbroad::errors::SbroadError),

    #[error("transaction: {0}")]
    Transaction(String),

    #[error("storage corrupted: failed to decode field '{field}' from table '{table}'")]
    StorageCorrupted { table: String, field: String },

    #[error("invalid configuration: {0}")]
    InvalidConfiguration(String),

    #[error(transparent)]
    Plugin(#[from] PluginError),

    #[error("{0}")]
    Unsupported(Unsupported),

    #[error("{0}")]
    Other(Box<dyn std::error::Error>),
}

struct DisplayIdOfInstance<'a>(pub &'a Result<RaftId, InstanceId>);
impl std::fmt::Display for DisplayIdOfInstance<'_> {
    #[inline]
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        match self.0 {
            Ok(raft_id) => write!(f, "raft_id {raft_id}"),
            Err(instance_id) => write!(f, "instance_id \"{instance_id}\""),
        }
    }
}

impl Error {
    pub fn error_code(&self) -> u32 {
        match self {
            Self::Tarantool(e) => e.error_code(),
            Self::Cas(e) => e.error_code(),
            Self::Raft(raft::Error::Store(raft::StorageError::Compacted)) => {
                ErrorCode::RaftLogCompacted as _
            }
            Self::Raft(raft::Error::Store(raft::StorageError::Unavailable))
            | Self::Raft(raft::Error::Store(raft::StorageError::LogTemporarilyUnavailable)) => {
                ErrorCode::RaftLogUnavailable as _
            }
            Self::Raft(_) => ErrorCode::Other as _,
            Self::Plugin(e) => e.error_code(),
            // TODO: when sbroad will need boxed errors, implement
            // `IntoBoxError` for `sbroad::errors::SbroadError` and
            // uncomment the following line:
            // Self::Sbroad(e) => e.error_code(),
            Self::LeaderUnknown => ErrorCode::LeaderUnknown as _,
            Self::NotALeader => ErrorCode::NotALeader as _,
            Self::TermMismatch { .. } => ErrorCode::TermMismatch as _,
            Self::NoSuchInstance(_) => ErrorCode::NoSuchInstance as _,
            Self::NoSuchReplicaset { .. } => ErrorCode::NoSuchReplicaset as _,
            // TODO: give other error types specific codes
            _ => ErrorCode::Other as _,
        }
    }

    #[inline(always)]
    pub fn other<E>(error: E) -> Self
    where
        E: Into<Box<dyn std::error::Error>>,
    {
        Self::Other(error.into())
    }

    #[inline(always)]
    pub fn invalid_configuration(msg: impl ToString) -> Self {
        Self::InvalidConfiguration(msg.to_string())
    }

    #[inline]
    pub fn is_retriable(&self) -> bool {
        let code = self.error_code();
        let Ok(code) = ErrorCode::try_from(code) else {
            return false;
        };
        code.is_retriable_for_cas()
    }
}

impl<E> From<timeout::Error<E>> for Error
where
    Error: From<E>,
{
    fn from(err: timeout::Error<E>) -> Self {
        match err {
            timeout::Error::Expired => Self::Timeout,
            timeout::Error::Failed(err) => err.into(),
        }
    }
}

impl From<::tarantool::network::ClientError> for Error {
    fn from(err: ::tarantool::network::ClientError) -> Self {
        Self::Tarantool(err.into())
    }
}

impl<E: Display> From<::tarantool::transaction::TransactionError<E>> for Error {
    fn from(err: ::tarantool::transaction::TransactionError<E>) -> Self {
        Self::Transaction(err.to_string())
    }
}

impl From<BoxError> for Error {
    #[inline(always)]
    fn from(err: BoxError) -> Self {
        Self::Tarantool(err.into())
    }
}

impl<V> From<tarantool::tlua::CallError<V>> for Error
where
    V: Into<tarantool::tlua::Void>,
{
    fn from(err: tarantool::tlua::CallError<V>) -> Self {
        Self::Lua(err.into())
    }
}

impl IntoBoxError for Error {
    #[inline(always)]
    fn error_code(&self) -> u32 {
        // Redirect to the inherent method
        Error::error_code(self)
    }

    #[inline]
    #[track_caller]
    fn into_box_error(self) -> BoxError {
        if let Self::Tarantool(e) = self {
            // Optimization
            return e.into_box_error();
        }

        // FIXME: currently these errors capture the source location of where this function is called (see #[track_caller]),
        // but we probably want to instead capture the location where the original error was created.
        BoxError::new(self.error_code(), self.to_string())
    }
}

////////////////////////////////////////////////////////////////////////////////
// ErrorInfo
////////////////////////////////////////////////////////////////////////////////

/// This is a serializable version of [`BoxError`].
///
/// TODO<https://git.picodata.io/picodata/picodata/tarantool-module/-/issues/221> just make BoxError serializable.
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize, PartialEq)]
pub struct ErrorInfo {
    pub error_code: u32,
    pub message: String,
    pub instance_id: InstanceId,
}

impl ErrorInfo {
    #[inline(always)]
    pub fn timeout(instance_id: impl Into<InstanceId>, message: impl Into<String>) -> Self {
        Self {
            error_code: TarantoolErrorCode::Timeout as _,
            message: message.into(),
            instance_id: instance_id.into(),
        }
    }

    #[inline(always)]
    pub fn new(instance_id: impl Into<InstanceId>, error: impl IntoBoxError) -> Self {
        let box_error = error.into_box_error();
        Self {
            error_code: box_error.error_code(),
            message: box_error.message().into(),
            instance_id: instance_id.into(),
        }
    }

    /// Should only be used in tests.
    pub fn for_tests() -> Self {
        Self {
            error_code: ErrorCode::Other as _,
            message: "there's a snake in my boot".into(),
            instance_id: InstanceId::from("i3378"),
        }
    }
}

impl std::fmt::Display for ErrorInfo {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        write!(f, "[instance_id:{}] ", self.instance_id)?;
        if let Some(c) = ErrorCode::from_i64(self.error_code as _) {
            write!(f, "{c:?}")?;
        } else if let Some(c) = TarantoolErrorCode::from_i64(self.error_code as _) {
            write!(f, "{c:?}")?;
        } else {
            write!(f, "#{}", self.error_code)?;
        }
        write!(f, ": {}", self.message)
    }
}

impl IntoBoxError for ErrorInfo {
    #[inline]
    fn error_code(&self) -> u32 {
        self.error_code
    }
}
