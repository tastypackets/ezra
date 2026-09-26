use caps::CapSet;
use nix::unistd::{Gid, Uid, User, getresgid, getresuid, setgroups, setresgid, setresuid};

use super::InitError;

/// Order matters: groups and gids can only be changed while still root.
pub fn drop_to(agent: &User, supplementary_groups: &[Gid]) -> Result<(), InitError> {
    setgroups(supplementary_groups).map_err(|source| InitError::DropPrivileges {
        step: "setgroups",
        source,
    })?;
    setresgid(agent.gid, agent.gid, agent.gid).map_err(|source| InitError::DropPrivileges {
        step: "setresgid",
        source,
    })?;
    setresuid(agent.uid, agent.uid, agent.uid).map_err(|source| InitError::DropPrivileges {
        step: "setresuid",
        source,
    })?;
    clear_inheritable_capabilities()?;
    ensure_root_cannot_be_regained(agent)
}

/// setuid clears the permitted, effective and ambient sets but not the inheritable one.
pub fn clear_inheritable_capabilities() -> Result<(), InitError> {
    caps::clear(None, CapSet::Inheritable)?;
    Ok(())
}

fn ensure_root_cannot_be_regained(agent: &User) -> Result<(), InitError> {
    let user_ids = getresuid().map_err(|source| InitError::DropPrivileges {
        step: "getresuid",
        source,
    })?;
    let group_ids = getresgid().map_err(|source| InitError::DropPrivileges {
        step: "getresgid",
        source,
    })?;
    let all_ids_are_the_agent = [user_ids.real, user_ids.effective, user_ids.saved]
        == [agent.uid; 3]
        && [group_ids.real, group_ids.effective, group_ids.saved] == [agent.gid; 3];

    if all_ids_are_the_agent
        && setresuid(Uid::from_raw(0), Uid::from_raw(0), Uid::from_raw(0)).is_err()
    {
        Ok(())
    } else {
        Err(InitError::PrivilegesStillRecoverable)
    }
}
