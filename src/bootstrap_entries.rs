use ::raft::prelude as raft;
use protobuf::Message;

use ::tarantool::msgpack;

use crate::access_control::validate_password;
use crate::config;
use crate::config::PicodataConfig;
use crate::instance::Instance;
use crate::replicaset::Replicaset;
use crate::schema;
use crate::schema::{ADMIN_ID, GUEST_ID};
use crate::storage::PropertyName;
use crate::storage::{Clusterwide, ClusterwideTable};
use crate::tier::Tier;
use crate::tlog;
use crate::traft;
use crate::traft::error::Error;
use crate::traft::op;
use std::collections::HashMap;
use std::env;
use tarantool::auth::{AuthData, AuthDef, AuthMethod};

pub(super) fn prepare(
    config: &PicodataConfig,
    instance: &Instance,
    tiers: &HashMap<String, Tier>,
    storage: &Clusterwide,
) -> Result<Vec<raft::Entry>, Error> {
    let mut init_entries = Vec::new();
    let mut ops = vec![];

    //
    // Populate "_pico_address" and "_pico_instance" with info about the first instance
    //
    ops.push(
        op::Dml::replace(
            ClusterwideTable::Address,
            &traft::PeerAddress {
                raft_id: instance.raft_id,
                address: config.instance.advertise_address().to_host_port(),
            },
            ADMIN_ID,
        )
        .expect("serialization cannot fail"),
    );
    ops.push(
        op::Dml::insert(ClusterwideTable::Instance, &instance, ADMIN_ID)
            .expect("serialization cannot fail"),
    );
    ops.push(
        op::Dml::insert(
            ClusterwideTable::Replicaset,
            &Replicaset::with_one_instance(instance),
            ADMIN_ID,
        )
        .expect("serialization cannot fail"),
    );
    let context = traft::EntryContext::Op(op::Op::BatchDml { ops });
    init_entries.push(
        traft::Entry {
            entry_type: raft::EntryType::EntryNormal,
            index: (init_entries.len() + 1) as _,
            term: traft::INIT_RAFT_TERM,
            data: vec![],
            context,
        }
        .into(),
    );

    //
    // Populate "_pico_tier" with initial tiers
    //
    let mut ops = vec![];
    for tier in tiers.values() {
        ops.push(
            op::Dml::insert(ClusterwideTable::Tier, &tier, ADMIN_ID)
                .expect("serialization cannot fail"),
        );
    }
    let context = traft::EntryContext::Op(op::Op::BatchDml { ops });
    init_entries.push(
        traft::Entry {
            entry_type: raft::EntryType::EntryNormal,
            index: (init_entries.len() + 1) as _,
            term: traft::INIT_RAFT_TERM,
            data: vec![],
            context,
        }
        .into(),
    );

    //
    // Populate "_pico_property" with initial values for cluster-wide properties
    //
    let mut ops = vec![];
    ops.push(
        op::Dml::insert(
            ClusterwideTable::Property,
            &(PropertyName::GlobalSchemaVersion, 0),
            ADMIN_ID,
        )
        .expect("serialization cannot fail"),
    );
    ops.push(
        op::Dml::insert(
            ClusterwideTable::Property,
            &(PropertyName::NextSchemaVersion, 1),
            ADMIN_ID,
        )
        .expect("serialization cannot fail"),
    );

    //
    // Populate "_pico_db_config" with initial values for cluster-wide properties
    //
    for (name, default) in config::get_defaults_for_all_alter_system_parameters() {
        #[rustfmt::skip]
        ops.push(
            op::Dml::insert(
                ClusterwideTable::DbConfig,
                &(name, default),
                ADMIN_ID,
            )
        .expect("serialization cannot fail"));
    }

    let context = traft::EntryContext::Op(op::Op::BatchDml { ops });
    init_entries.push(
        traft::Entry {
            entry_type: raft::EntryType::EntryNormal,
            index: (init_entries.len() + 1) as _,
            term: traft::INIT_RAFT_TERM,
            data: vec![],
            context,
        }
        .into(),
    );

    //
    // Populate "_pico_user" and "_pico_priv" to match tarantool ones
    //
    // Note: op::Dml is used instead of op::Acl because with Acl
    // replicas will attempt to apply these records to corresponding
    // tarantool spaces which is not needed
    let mut ops = vec![];
    for (user_def, privilege_defs) in &schema::system_user_definitions() {
        ops.push(
            op::Dml::insert(ClusterwideTable::User, user_def, ADMIN_ID)
                .expect("serialization cannot fail"),
        );

        for priv_def in privilege_defs {
            ops.push(
                op::Dml::insert(ClusterwideTable::Privilege, priv_def, ADMIN_ID)
                    .expect("serialization cannot fail"),
            );
        }
    }

    //
    // Populate "_pico_user" and "_pico_priv" to match tarantool ones
    //
    for (role_def, privilege_defs) in &schema::system_role_definitions() {
        ops.push(
            op::Dml::insert(ClusterwideTable::User, role_def, ADMIN_ID)
                .expect("serialization cannot fail"),
        );

        for priv_def in privilege_defs {
            ops.push(
                op::Dml::insert(ClusterwideTable::Privilege, priv_def, ADMIN_ID)
                    .expect("serialization cannot fail"),
            );
        }
    }
    let context = traft::EntryContext::Op(op::Op::BatchDml { ops });
    init_entries.push(
        traft::Entry {
            entry_type: raft::EntryType::EntryNormal,
            index: (init_entries.len() + 1) as _,
            term: traft::INIT_RAFT_TERM,
            data: vec![],
            context,
        }
        .into(),
    );

    //
    // Set up password for admin from environment variable
    //
    if let Some(var_password) = env::var_os("PICODATA_ADMIN_PASSWORD") {
        let password = var_password.into_string().map_err(|e| {
            Error::other(format!(
                "Failed to convert OsString: {}",
                e.into_string().unwrap()
            ))
        })?;

        let method = AuthMethod::Md5;
        let name = "admin";
        validate_password(&password, &method, storage)?;
        let data = AuthData::new(&method, name, &password);
        let auth = AuthDef::new(method, data.into_string());

        let op_elem = op::Op::Acl(op::Acl::ChangeAuth {
            user_id: ADMIN_ID,
            auth,
            initiator: ADMIN_ID,
            schema_version: 1,
        });

        let context = traft::EntryContext::Op(op_elem);
        init_entries.push(
            traft::Entry {
                entry_type: raft::EntryType::EntryNormal,
                index: (init_entries.len() + 1) as _,
                term: traft::INIT_RAFT_TERM,
                data: vec![],
                context,
            }
            .into(),
        );
        tlog!(Info, "Password for user=admin has been set successfully");
    }

    let op_elem = op::Op::Acl(op::Acl::ChangeAuth {
        user_id: GUEST_ID,
        auth: AuthDef::new(
            AuthMethod::Md5,
            AuthData::new(&AuthMethod::Md5, "guest", "").into_string(),
        ),
        initiator: ADMIN_ID,
        schema_version: 1,
    });

    let context = traft::EntryContext::Op(op_elem);
    init_entries.push(
        traft::Entry {
            entry_type: raft::EntryType::EntryNormal,
            index: (init_entries.len() + 1) as _,
            term: traft::INIT_RAFT_TERM,
            data: vec![],
            context,
        }
        .into(),
    );

    //
    // Populate "_pico_table" & "_pico_index" with definitions of builtins
    //
    let mut ops = vec![];
    for (table_def, index_defs) in schema::system_table_definitions() {
        ops.push(
            op::Dml::insert_raw(
                ClusterwideTable::Table,
                msgpack::encode(&table_def),
                ADMIN_ID,
            )
            .expect("serialization cannot fail"),
        );
        for index_def in index_defs {
            ops.push(
                op::Dml::insert(ClusterwideTable::Index, &index_def, ADMIN_ID)
                    .expect("serialization cannot fail"),
            );
        }
    }
    let context = traft::EntryContext::Op(op::Op::BatchDml { ops });
    init_entries.push(
        traft::Entry {
            entry_type: raft::EntryType::EntryNormal,
            index: (init_entries.len() + 1) as _,
            term: traft::INIT_RAFT_TERM,
            data: vec![],
            context,
        }
        .into(),
    );

    //
    // Initial raft configuration
    //
    init_entries.push({
        let conf_change = raft::ConfChange {
            change_type: raft::ConfChangeType::AddNode,
            node_id: instance.raft_id,
            ..Default::default()
        };
        let e = traft::Entry {
            entry_type: raft::EntryType::EntryConfChange,
            index: (init_entries.len() + 1) as _,
            term: traft::INIT_RAFT_TERM,
            data: conf_change.write_to_bytes().unwrap(),
            context: traft::EntryContext::None,
        };

        e.into()
    });

    Ok(init_entries)
}
