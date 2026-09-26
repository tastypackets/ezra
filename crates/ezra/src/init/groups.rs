use nix::unistd::{Gid, Uid, User, getgroups};

use super::InitError;
use super::privileges::UserExt;

/// The groups the agent keeps after the switch.
#[derive(Debug, PartialEq, Eq)]
pub struct SupplementaryGroups(Vec<Gid>);

impl SupplementaryGroups {
    pub fn for_agent(agent: &User) -> Result<Self, InitError> {
        let agent_groups = agent.group_ids()?;
        let current_groups = getgroups().map_err(|source| InitError::Lookup {
            subject: "the current supplementary groups".to_owned(),
            source,
        })?;
        let invoking_user_image_groups = match User::from_uid(Uid::effective()) {
            Ok(Some(invoking_user)) => invoking_user.group_ids()?,
            Ok(None) | Err(_) => Vec::new(),
        };
        Ok(Self::merge(
            &agent_groups,
            &current_groups,
            &invoking_user_image_groups,
        ))
    }

    /// The agent's own groups plus groups the container runtime added (e.g. `docker run --group-add`),
    /// never gid 0 and never groups the image's /etc/group grants the invoking user (usually root).
    fn merge(
        agent_groups: &[Gid],
        current_groups: &[Gid],
        invoking_user_image_groups: &[Gid],
    ) -> Self {
        let runtime_added_groups = current_groups
            .iter()
            .filter(|group| !invoking_user_image_groups.contains(group));
        let mut merged: Vec<Gid> = agent_groups
            .iter()
            .chain(runtime_added_groups)
            .copied()
            .filter(|group| group.as_raw() != 0)
            .collect();
        merged.sort_unstable_by_key(|group| group.as_raw());
        merged.dedup();
        Self(merged)
    }

    pub fn as_slice(&self) -> &[Gid] {
        &self.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn gids(raw_ids: &[u32]) -> Vec<Gid> {
        raw_ids.iter().copied().map(Gid::from_raw).collect()
    }

    fn merge(agent: &[u32], current: &[u32], image: &[u32]) -> Vec<Gid> {
        SupplementaryGroups::merge(&gids(agent), &gids(current), &gids(image)).0
    }

    #[test]
    fn keeps_the_agent_groups() {
        assert_eq!(merge(&[1000, 27], &[0], &[0]), gids(&[27, 1000]));
    }

    #[test]
    fn keeps_groups_added_by_the_container_runtime() {
        assert_eq!(merge(&[1000], &[0, 4242], &[0]), gids(&[1000, 4242]));
    }

    #[test]
    fn drops_groups_the_image_grants_root() {
        let alpine_root_groups = [0, 1, 2, 3, 4, 6, 10, 11, 20, 26, 27];
        let current_groups = [0, 1, 2, 3, 4, 6, 10, 11, 20, 26, 27, 4242];

        assert_eq!(
            merge(&[1000], &current_groups, &alpine_root_groups),
            gids(&[1000, 4242])
        );
    }

    #[test]
    fn never_includes_group_zero() {
        assert_eq!(merge(&[0, 1000], &[0], &[]), gids(&[1000]));
    }

    #[test]
    fn lists_each_group_once() {
        assert_eq!(
            merge(&[1000, 1000], &[1000, 4242], &[0]),
            gids(&[1000, 4242])
        );
    }
}
