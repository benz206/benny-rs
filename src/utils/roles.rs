//! Shared role-hierarchy helpers used by the roles and welcome cogs.

use serenity::all::{Role, RoleId};
use std::cmp::Reverse;
use std::collections::HashMap;

/// Ordering key reproducing Discord's role hierarchy: a role is "lower" when its
/// position is lower, or — at equal position — when its id is larger (created
/// later). `role_rank(a) < role_rank(b)` means `a` sits below `b`.
pub fn role_rank(r: &Role) -> (i64, Reverse<u64>) {
    (r.position as i64, Reverse(r.id.get()))
}

/// The highest role a member holds, always including `@everyone` (id == guild
/// id) so the result is never `None` for a member of the guild.
pub fn top_role<'a>(
    member_roles: &[RoleId],
    roles: &'a HashMap<RoleId, Role>,
    everyone_id: RoleId,
) -> Option<&'a Role> {
    member_roles
        .iter()
        .filter_map(|rid| roles.get(rid))
        .chain(roles.get(&everyone_id))
        .max_by_key(|r| role_rank(r))
}
