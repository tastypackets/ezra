use std::ffi::CString;

use caps::CapSet;
use nix::unistd::{
    Gid, Uid, User, getgrouplist, getresgid, getresuid, setgroups, setresgid, setresuid,
};

use super::InitError;
use super::groups::SupplementaryGroups;

pub trait UserExt: Sized {
    fn look_up_by_name(user_name: &str) -> Result<Option<Self>, InitError>;

    /// The user's groups from /etc/group, including its primary group.
    fn group_ids(&self) -> Result<Vec<Gid>, InitError>;

    /// Switches this process to the user for good: groups, then gids, then uids, while still root.
    fn switch_process_to(
        &self,
        supplementary_groups: &SupplementaryGroups,
    ) -> Result<(), InitError>;
}

impl UserExt for User {
    fn look_up_by_name(user_name: &str) -> Result<Option<Self>, InitError> {
        Self::from_name(user_name).map_err(|source| InitError::Lookup {
            subject: format!("user {user_name:?}"),
            source,
        })
    }

    fn group_ids(&self) -> Result<Vec<Gid>, InitError> {
        let user_name = CString::new(self.name.as_str())?;
        getgrouplist(&user_name, self.gid).map_err(|source| InitError::Lookup {
            subject: format!("the groups of {:?}", self.name),
            source,
        })
    }

    fn switch_process_to(
        &self,
        supplementary_groups: &SupplementaryGroups,
    ) -> Result<(), InitError> {
        let step_failed = |step| move |source| InitError::DropPrivileges { step, source };
        setgroups(supplementary_groups.as_slice()).map_err(step_failed("setgroups"))?;
        setresgid(self.gid, self.gid, self.gid).map_err(step_failed("setresgid"))?;
        setresuid(self.uid, self.uid, self.uid).map_err(step_failed("setresuid"))?;
        Capabilities::clear_inheritable()?;

        let user_ids = getresuid().map_err(step_failed("getresuid"))?;
        let group_ids = getresgid().map_err(step_failed("getresgid"))?;
        let all_ids_are_the_user = [user_ids.real, user_ids.effective, user_ids.saved]
            == [self.uid; 3]
            && [group_ids.real, group_ids.effective, group_ids.saved] == [self.gid; 3];
        let root_is_unreachable =
            setresuid(Uid::from_raw(0), Uid::from_raw(0), Uid::from_raw(0)).is_err();
        if all_ids_are_the_user && root_is_unreachable {
            Ok(())
        } else {
            Err(InitError::PrivilegesStillRecoverable)
        }
    }
}

/// This process's capability sets.
pub struct Capabilities;

impl Capabilities {
    /// setuid clears the permitted, effective and ambient sets but not the inheritable one.
    pub fn clear_inheritable() -> Result<(), InitError> {
        caps::clear(None, CapSet::Inheritable)?;
        Ok(())
    }
}
