use std::collections::BTreeSet;

use asterism_domain::{Permission, Role, UserId};

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Principal {
    pub user_id: UserId,
    permissions: BTreeSet<Permission>,
}

impl Principal {
    pub fn from_roles(
        user_id: UserId,
        roles: impl IntoIterator<Item = Role>,
        explicit_permissions: impl IntoIterator<Item = Permission>,
    ) -> Self {
        let mut permissions: BTreeSet<_> = explicit_permissions.into_iter().collect();
        for role in roles {
            permissions.extend(role_permissions(role));
        }
        Self {
            user_id,
            permissions,
        }
    }

    pub fn has(&self, permission: Permission) -> bool {
        self.permissions.contains(&permission)
    }

    /// Returns the effective permissions after role grants and explicit grants
    /// have been merged.
    pub fn permissions(&self) -> &BTreeSet<Permission> {
        &self.permissions
    }

    /// Requires one permission without consulting role names.
    ///
    /// # Errors
    ///
    /// Returns [`AuthorizationError::MissingPermission`] if the permission set
    /// does not include the requested action.
    pub fn require(&self, permission: Permission) -> Result<(), AuthorizationError> {
        if self.has(permission) {
            Ok(())
        } else {
            Err(AuthorizationError::MissingPermission(permission))
        }
    }

    /// Authorizes an owner-scoped resource using either the self or global
    /// permission.
    ///
    /// # Errors
    ///
    /// Returns [`AuthorizationError::MissingPermission`] when neither route is
    /// allowed.
    pub fn require_owner_or_any(
        &self,
        owner_id: UserId,
        own_permission: Permission,
        any_permission: Permission,
    ) -> Result<(), AuthorizationError> {
        if (self.user_id == owner_id && self.has(own_permission)) || self.has(any_permission) {
            Ok(())
        } else {
            Err(AuthorizationError::MissingPermission(
                if self.user_id == owner_id {
                    own_permission
                } else {
                    any_permission
                },
            ))
        }
    }
}

fn role_permissions(role: Role) -> &'static [Permission] {
    use Permission::{
        ExecuteAnyTask, ExecuteOwnTasks, GrantCredits, ManageCredits, ManageOwnAccounts,
        ManagePricing, ManageProviders, ManageSystem, ManageUsers, ReadOwnCredits, ReadOwnTasks,
        ReadProviders, ViewAnyAudit, ViewOwnAudit,
    };
    match role {
        Role::Master => &[
            ReadOwnTasks,
            ReadProviders,
            ExecuteOwnTasks,
            ManageOwnAccounts,
            ReadOwnCredits,
            ViewOwnAudit,
            ManageUsers,
            ManageProviders,
            ManageCredits,
            GrantCredits,
            ManagePricing,
            ManageSystem,
            ExecuteAnyTask,
            ViewAnyAudit,
        ],
        Role::Operator => &[
            ReadOwnTasks,
            ReadProviders,
            ExecuteOwnTasks,
            ManageOwnAccounts,
            ReadOwnCredits,
            ViewOwnAudit,
            ManageProviders,
            ExecuteAnyTask,
            ViewAnyAudit,
        ],
        Role::User => &[
            ReadOwnTasks,
            ReadProviders,
            ExecuteOwnTasks,
            ManageOwnAccounts,
            ReadOwnCredits,
            ViewOwnAudit,
        ],
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum AuthorizationError {
    #[error("required permission is missing: {0:?}")]
    MissingPermission(Permission),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn role_is_resolved_once_into_permissions() {
        let master = Principal::from_roles(UserId::new(), [Role::Master], []);
        assert!(master.has(Permission::ManageSystem));
        assert!(master.has(Permission::GrantCredits));
        assert!(master.permissions().contains(&Permission::ManageSystem));

        let user = Principal::from_roles(UserId::new(), [Role::User], []);
        assert!(user.has(Permission::ExecuteOwnTasks));
        assert!(!user.has(Permission::ExecuteAnyTask));
    }

    #[test]
    fn owner_scope_does_not_grant_access_to_another_user() {
        let principal = Principal::from_roles(UserId::new(), [Role::User], []);
        assert_eq!(
            principal.require_owner_or_any(
                UserId::new(),
                Permission::ExecuteOwnTasks,
                Permission::ExecuteAnyTask
            ),
            Err(AuthorizationError::MissingPermission(
                Permission::ExecuteAnyTask
            ))
        );
    }
}
