use crate::instance::Instance;
use crate::instance::InstanceName;
use crate::pico_service::pico_service_password;
use crate::replicaset::Replicaset;
use crate::replicaset::ReplicasetName;
use crate::replicaset::Weight;
use crate::schema::PICO_SERVICE_USER_NAME;
use crate::storage::Clusterwide;
use crate::storage::ToEntryIter as _;
use crate::traft::error::Error;
use crate::traft::RaftId;
use std::collections::HashMap;
use tarantool::tlua;

pub fn get_replicaset_priority_list(
    tier: &str,
    replicaset_uuid: &str,
) -> Result<Vec<InstanceName>, Error> {
    let lua = tarantool::lua_state();
    let pico: tlua::LuaTable<_> = lua
        .get("pico")
        .ok_or_else(|| Error::other("pico lua module disappeared"))?;

    let func: tlua::LuaFunction<_> = pico.try_get("_replicaset_priority_list")?;
    func.call_with_args((tier, replicaset_uuid))
        .map_err(|e| tlua::LuaError::from(e).into())
}

/// Returns the replicaset uuid and an array of replicas in descending priority
/// order.
pub fn get_replicaset_uuid_by_bucket_id(tier: &str, bucket_id: u64) -> Result<String, Error> {
    let lua = tarantool::lua_state();
    let pico: tlua::LuaTable<_> = lua
        .get("pico")
        .ok_or_else(|| Error::other("pico lua module disappeared"))?;

    let func: tlua::LuaFunction<_> = pico.try_get("_replicaset_uuid_by_bucket_id")?;
    func.call_with_args((tier, bucket_id))
        .map_err(|e| tlua::LuaError::from(e).into())
}

#[rustfmt::skip]
#[derive(serde::Serialize, serde::Deserialize)]
#[derive(Default, Clone, Debug, PartialEq, tlua::PushInto, tlua::Push, tlua::LuaRead)]
pub struct VshardConfig {
    sharding: HashMap<String, ReplicasetSpec>,
    discovery_mode: DiscoveryMode,

    /// This field is not stored in the global storage, instead
    /// it is set right before the config is passed into vshard.*.cfg,
    /// otherwise vshard will override it with an incorrect value.
    #[serde(skip_serializing_if="Option::is_none")]
    #[serde(default)]
    pub listen: Option<String>,
}

#[rustfmt::skip]
#[derive(serde::Serialize, serde::Deserialize)]
#[derive(Default, Clone, Debug, PartialEq, tlua::PushInto, tlua::Push, tlua::LuaRead)]
struct ReplicasetSpec {
    replicas: HashMap<String, ReplicaSpec>,
    weight: Option<Weight>,
}

#[rustfmt::skip]
#[derive(serde::Serialize, serde::Deserialize)]
#[derive(Default, Clone, Debug, PartialEq, Eq, tlua::PushInto, tlua::Push, tlua::LuaRead)]
struct ReplicaSpec {
    uri: String,
    name: String,
    master: bool,
}

tarantool::define_str_enum! {
    /// Specifies the mode of operation for the bucket discovery fiber of vshard
    /// router.
    ///
    /// See [`vshard.router.discovery_set`] for more details.
    ///
    /// [`vshard.router.discovery_set`]: https://www.tarantool.io/en/doc/latest/reference/reference_rock/vshard/vshard_router/#router-api-discovery-set
    #[derive(Default)]
    pub enum DiscoveryMode {
        Off = "off",
        #[default]
        On = "on",
        Once = "once",
    }
}

impl VshardConfig {
    pub fn from_storage(storage: &Clusterwide, tier_name: &str) -> Result<Self, Error> {
        let instances = storage.instances.all_instances()?;
        let peer_addresses: HashMap<_, _> = storage
            .peer_addresses
            .iter()?
            .map(|pa| (pa.raft_id, pa.address))
            .collect();
        let replicasets: Vec<_> = storage.replicasets.iter()?.collect();
        let replicasets: HashMap<_, _> = replicasets.iter().map(|rs| (&rs.name, rs)).collect();

        let result = Self::new(&instances, &peer_addresses, &replicasets, tier_name);
        Ok(result)
    }

    pub fn new(
        instances: &[Instance],
        peer_addresses: &HashMap<RaftId, String>,
        replicasets: &HashMap<&ReplicasetName, &Replicaset>,
        tier_name: &str,
    ) -> Self {
        let mut sharding: HashMap<String, ReplicasetSpec> = HashMap::new();
        for peer in instances {
            if !peer.may_respond() || peer.tier != tier_name {
                continue;
            }
            let Some(address) = peer_addresses.get(&peer.raft_id) else {
                crate::tlog!(Warning, "skipping instance: address not found for peer";
                    "raft_id" => peer.raft_id,
                );
                continue;
            };
            let Some(r) = replicasets.get(&peer.replicaset_name) else {
                crate::tlog!(Debug, "skipping instance: replicaset not initialized yet";
                    "instance_name" => %peer.name,
                );
                continue;
            };

            let replicaset = sharding
                .entry(peer.replicaset_uuid.clone())
                .or_insert_with(|| ReplicasetSpec {
                    weight: Some(r.weight),
                    ..Default::default()
                });

            replicaset.replicas.insert(
                peer.uuid.clone(),
                ReplicaSpec {
                    uri: format!("{PICO_SERVICE_USER_NAME}@{address}"),
                    master: r.current_master_name == peer.name,
                    name: peer.name.to_string(),
                },
            );
        }

        Self {
            listen: None,
            sharding,
            discovery_mode: DiscoveryMode::On,
        }
    }

    /// Set the `pico_service` password in the uris of all the replicas in this
    /// config. This is needed, because currently we store this config in a
    /// global table, but we don't want to store the passwords in plain text, so
    /// we only set them after the config is deserialized and before the config
    /// is applied.
    pub fn set_password_in_uris(&mut self) {
        let password = pico_service_password();

        for replicaset in self.sharding.values_mut() {
            for replica in replicaset.replicas.values_mut() {
                // FIXME: this is kinda stupid, we could as easily store the
                // username and address in separate fields, or even not store
                // the username at all, because it's always `pico_service`,
                // but this also works... for now.
                let Some((username, address)) = replica.uri.split_once('@') else {
                    continue;
                };
                if username != PICO_SERVICE_USER_NAME {
                    continue;
                }
                replica.uri = format!("{username}:{password}@{address}");
            }
        }
    }
}
