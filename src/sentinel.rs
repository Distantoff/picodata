use crate::has_states;
use crate::instance::StateVariant::*;
use crate::proc_name;
use crate::reachability::InstanceReachabilityManagerRef;
use crate::rpc;
use crate::rpc::update_instance::proc_update_instance;
use crate::storage::Clusterwide;
use crate::tlog;
use crate::traft::error::Error;
use crate::traft::network::ConnectionPool;
use crate::traft::{node, RaftSpaceAccess};
use ::tarantool::fiber;
use ::tarantool::fiber::r#async::timeout::IntoTimeout as _;
use ::tarantool::fiber::r#async::watch;
use std::ops::ControlFlow;
use std::rc::Rc;
use std::time::Duration;

impl Loop {
    /// A value for non-urgent timeouts, e.g. nothing needed to be done during
    /// a loop iteration.
    const SENTINEL_LONG_SLEEP: Duration = Duration::from_secs(1);

    /// A value for urgent timeouts, e.g. retry of failed update peer request.
    const SENTINEL_SHORT_RETRY: Duration = Duration::from_millis(300);

    const UPDATE_INSTANCE_TIMEOUT: Duration = Duration::from_secs(3);

    async fn iter_fn(
        State {
            pool,
            storage,
            raft_storage,
            raft_status,
            status,
            instance_reachability,
        }: &mut State,
    ) -> ControlFlow<()> {
        if status.get() == SentinelStatus::Initial || node::global().is_err() {
            tlog!(Debug, "waiting until initialized...");
            _ = status.changed().timeout(Self::SENTINEL_LONG_SLEEP).await;
            return ControlFlow::Continue(());
        }

        let node = node::global().expect("just checked it's ok");
        let cluster_name = raft_storage.cluster_name().expect("storage shouldn't fail");

        ////////////////////////////////////////////////////////////////////////
        // Awoken during graceful shutdown.
        // Should change own target state to Offline and finish.
        if status.get() == SentinelStatus::ShuttingDown {
            let raft_id = node.raft_id();
            let Ok(instance) = storage.instances.get(&raft_id) else {
                // This can happen if for example a snapshot arrives
                // and we truncate _pico_instance (read uncommitted btw).
                // In this case we also just wait some more.
                _ = status.changed().timeout(Self::SENTINEL_SHORT_RETRY).await;
                return ControlFlow::Continue(());
            };

            if has_states!(instance, * -> Expelled) {
                tlog!(Debug, "instance has been expelled, sentinel out");
                return ControlFlow::Break(());
            }

            let req = rpc::update_instance::Request::new(instance.name, cluster_name)
                .with_target_state(Offline);

            tlog!(Info, "setting own target state Offline");
            let timeout = Self::SENTINEL_SHORT_RETRY;
            loop {
                let now = fiber::clock();
                let res = async {
                    let Some(leader_id) = raft_status.get().leader_id else {
                        return Err(Error::LeaderUnknown);
                    };
                    pool.call(&leader_id, proc_name!(proc_update_instance), &req, timeout)?
                        .await?;
                    Ok(())
                }
                .await;
                match res {
                    Ok(_) => return ControlFlow::Break(()),
                    Err(e) => {
                        tlog!(Warning,
                            "failed setting own target state Offline: {e}, retrying ...";
                        );
                        fiber::sleep(timeout.saturating_sub(now.elapsed()));
                        continue;
                    }
                }
            }
        }

        ////////////////////////////////////////////////////////////////////////
        // When running on leader, find any unreachable instances which need to
        // have their state automatically changed.
        if raft_status.get().raft_state.is_leader() {
            let instances = storage
                .instances
                .all_instances()
                .expect("storage shouldn't fail");
            let unreachables = instance_reachability.borrow().get_unreachables();
            let mut instance_to_downgrade = None;
            for instance in &instances {
                if has_states!(instance, * -> Online) && unreachables.contains(&instance.raft_id) {
                    instance_to_downgrade = Some(instance);
                }
            }
            let Some(instance) = instance_to_downgrade else {
                _ = status.changed().timeout(Self::SENTINEL_LONG_SLEEP).await;
                return ControlFlow::Continue(());
            };

            tlog!(Info, "setting target state Offline"; "instance_name" => %instance.name);
            let req = rpc::update_instance::Request::new(instance.name.clone(), cluster_name)
                // We only try setting the state once and if a CaS conflict
                // happens we should reassess the situation, because somebody
                // else could have changed this particular instance's target state.
                .with_dont_retry(true)
                .with_target_state(Offline);
            let res = rpc::update_instance::handle_update_instance_request_and_wait(
                req,
                Self::UPDATE_INSTANCE_TIMEOUT,
            );
            if let Err(e) = res {
                tlog!(Warning,
                    "failed setting target state Offline: {e}";
                    "instance_name" => %instance.name,
                );
            }

            _ = status.changed().timeout(Self::SENTINEL_SHORT_RETRY).await;
            return ControlFlow::Continue(());
        }

        ////////////////////////////////////////////////////////////////////////
        // When running not on leader, check if own target has automatically
        // changed to Offline and try to update it to Online.
        let raft_id = node.raft_id();
        let Ok(instance) = storage.instances.get(&raft_id) else {
            // This can happen if for example a snapshot arrives
            // and we truncate _pico_instance (read uncommitted btw).
            // In this case we also just wait some more.
            _ = status.changed().timeout(Self::SENTINEL_SHORT_RETRY).await;
            return ControlFlow::Continue(());
        };

        if has_states!(instance, * -> Offline) {
            tlog!(Info, "setting own target state Online");
            let req = rpc::update_instance::Request::new(instance.name.clone(), cluster_name)
                // We only try setting the state once and if a CaS conflict
                // happens we should reassess the situation, because somebody
                // else could have changed this particular instance's target state.
                .with_dont_retry(true)
                .with_target_state(Online);
            let res = async {
                let Some(leader_id) = raft_status.get().leader_id else {
                    return Err(Error::LeaderUnknown);
                };
                pool.call(
                    &leader_id,
                    proc_name!(proc_update_instance),
                    &req,
                    Self::UPDATE_INSTANCE_TIMEOUT,
                )?
                .await?;
                Ok(())
            }
            .await;
            if let Err(e) = res {
                tlog!(Warning, "failed setting own target state Online: {e}");
            }

            _ = status.changed().timeout(Self::SENTINEL_SHORT_RETRY).await;
            return ControlFlow::Continue(());
        }

        _ = status.changed().timeout(Self::SENTINEL_LONG_SLEEP).await;
        return ControlFlow::Continue(());
    }

    pub fn start(
        pool: Rc<ConnectionPool>,
        raft_status: watch::Receiver<node::Status>,
        storage: Clusterwide,
        raft_storage: RaftSpaceAccess,
        instance_reachability: InstanceReachabilityManagerRef,
    ) -> Self {
        let (status_tx, status_rx) = watch::channel(SentinelStatus::Initial);

        let state = State {
            pool,
            storage,
            raft_storage,
            raft_status,
            status: status_rx,
            instance_reachability,
        };

        Self {
            fiber_id: crate::loop_start!("sentinel_loop", Self::iter_fn, state),
            status: status_tx,
        }
    }

    #[inline]
    pub fn for_tests() -> Self {
        let (status, _) = watch::channel(SentinelStatus::Initial);
        Self {
            fiber_id: 0,
            status,
        }
    }

    pub fn on_shut_down(&self) {
        self.status
            .send(SentinelStatus::ShuttingDown)
            .expect("we shouldn't be holding references to the value")
    }

    pub fn on_self_activate(&self) {
        self.status
            .send(SentinelStatus::Activated)
            .expect("we shouldn't be holding references to the value")
    }
}

pub struct Loop {
    #[allow(dead_code)]
    fiber_id: fiber::FiberId,
    status: watch::Sender<SentinelStatus>,
}

/// Describes possible states of the current instance with respect to what
/// sentinel should be doing.
///
/// TODO: maybe this should be merged with [`node::Status`].
#[derive(Debug, Default, Copy, Clone, PartialEq, Eq)]
enum SentinelStatus {
    /// Instance has started, but didn't yet receive confirmation from the
    /// leader that it was activated.
    #[default]
    Initial,

    /// Instance has been activated, sentinel is doing it's normal job.
    Activated,

    /// Instance is currently gracefully shutting down.
    ShuttingDown,
}

struct State {
    pool: Rc<ConnectionPool>,
    storage: Clusterwide,
    raft_storage: RaftSpaceAccess,
    raft_status: watch::Receiver<node::Status>,
    status: watch::Receiver<SentinelStatus>,
    instance_reachability: InstanceReachabilityManagerRef,
}
