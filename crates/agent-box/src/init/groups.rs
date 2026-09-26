use std::ffi::CString;

use nix::unistd::{Gid, Uid, User, getgrouplist, getgroups};

use super::InitError;

pub fn supplementary_groups_for(agent: &User) -> Result<Vec<Gid>, InitError> {
    let agent_groups = group_list_of(agent)?;
    let current_groups = getgroups().map_err(|source| InitError::Lookup {
        subject: "the current supplementary groups".to_owned(),
        source,
    })?;
    let invoking_user_image_groups = match User::from_uid(Uid::effective()) {
        Ok(Some(invoking_user)) => group_list_of(&invoking_user)?,
        Ok(None) | Err(_) => Vec::new(),
    };

    let merged = merge_supplementary_groups(
        &as_raw_ids(&agent_groups),
        &as_raw_ids(&current_groups),
        &as_raw_ids(&invoking_user_image_groups),
    );
    Ok(merged.into_iter().map(Gid::from_raw).collect())
}

/// The agent's own groups plus groups the container runtime added (e.g. `docker run --group-add`),
/// never gid 0 and never groups the image's /etc/group grants the invoking user (usually root).
pub fn merge_supplementary_groups(
    agent_groups: &[u32],
    current_groups: &[u32],
    invoking_user_image_groups: &[u32],
) -> Vec<u32> {
    let runtime_added_groups = current_groups
        .iter()
        .filter(|group| !invoking_user_image_groups.contains(group));

    let mut merged: Vec<u32> = agent_groups
        .iter()
        .chain(runtime_added_groups)
        .copied()
        .filter(|&group| group != 0)
        .collect();
    merged.sort_unstable();
    merged.dedup();
    merged
}

fn group_list_of(user: &User) -> Result<Vec<Gid>, InitError> {
    let user_name = CString::new(user.name.as_str())?;
    getgrouplist(&user_name, user.gid).map_err(|source| InitError::Lookup {
        subject: format!("the groups of {:?}", user.name),
        source,
    })
}

fn as_raw_ids(groups: &[Gid]) -> Vec<u32> {
    groups.iter().map(|group| group.as_raw()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keeps_the_agent_groups() {
        assert_eq!(
            merge_supplementary_groups(&[1000, 27], &[0], &[0]),
            [27, 1000]
        );
    }

    #[test]
    fn keeps_groups_added_by_the_container_runtime() {
        assert_eq!(
            merge_supplementary_groups(&[1000], &[0, 4242], &[0]),
            [1000, 4242]
        );
    }

    #[test]
    fn drops_groups_the_image_grants_root() {
        let alpine_root_groups = [0, 1, 2, 3, 4, 6, 10, 11, 20, 26, 27];
        let current_groups = [0, 1, 2, 3, 4, 6, 10, 11, 20, 26, 27, 4242];

        assert_eq!(
            merge_supplementary_groups(&[1000], &current_groups, &alpine_root_groups),
            [1000, 4242]
        );
    }

    #[test]
    fn never_includes_group_zero() {
        assert_eq!(merge_supplementary_groups(&[0, 1000], &[0], &[]), [1000]);
    }

    #[test]
    fn lists_each_group_once() {
        assert_eq!(
            merge_supplementary_groups(&[1000, 1000], &[1000, 4242], &[0]),
            [1000, 4242]
        );
    }
}
