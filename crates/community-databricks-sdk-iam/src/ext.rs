//! Go's SCIM convenience methods (`service/iam/ext_api.go`) on the V2
//! services: `get_by_id`, `delete_by_id`, a name → ID map, and a lookup by
//! user or display name. The v1 services Go keeps (`w.Users` and so on)
//! call the same endpoints; `users()`, `groups()` and
//! `service_principals()` on the clients return these V2 services.

use std::collections::BTreeMap;

use community_databricks_core::Result;
use community_databricks_core::lookup;

use crate::{
    AccountGroup, AccountGroupsV2Api, AccountServicePrincipal, AccountServicePrincipalsV2Api,
    AccountUser, AccountUsersV2Api, DeleteAccountGroupRequest,
    DeleteAccountServicePrincipalRequest, DeleteAccountUserRequest, DeleteGroupRequest,
    DeleteServicePrincipalRequest, DeleteUserRequest, GetAccountGroupRequest,
    GetAccountServicePrincipalRequest, GetAccountUserRequest, GetGroupRequest,
    GetServicePrincipalRequest, GetUserRequest, Group, GroupsV2Api, ListAccountGroupsRequest,
    ListAccountServicePrincipalsRequest, ListAccountUsersRequest, ListGroupsRequest,
    ListServicePrincipalsRequest, ListUsersRequest, ServicePrincipal, ServicePrincipalsV2Api, User,
    UsersV2Api,
};

/// A SCIM filter string literal: quoted, with `\` and `"` escaped.
fn scim_string(value: &str) -> String {
    format!("\"{}\"", value.replace('\\', "\\\\").replace('"', "\\\""))
}

/// `get_by_id`, `delete_by_id`, `<map>` and `<get_by>` for one SCIM service.
macro_rules! scim_helpers {
    (
        $api:ty, $item:ty, $what:literal,
        get = $get:ty, delete = $delete:ty, list = $list:ty,
        key = $key:ident, attr = $attr:literal, map = $map:ident, get_by = $get_by:ident,
        go = $go:literal
    ) => {
        impl $api {
            #[doc = concat!("Get a ", $what, " by ID (Go: `", $go, ".GetById`).")]
            pub async fn get_by_id(&self, id: impl Into<String>) -> Result<$item> {
                self.get(<$get>::new(id)).await
            }

            #[doc = concat!("Delete a ", $what, " by ID (Go: `", $go, ".DeleteById`).")]
            pub async fn delete_by_id(&self, id: impl Into<String>) -> Result<()> {
                self.delete(<$delete>::new(id)).await
            }

            #[doc = concat!(
                "Map each ", $what, "'s `", stringify!($key), "` to its `id`, listing them all \
                 first (Go: `", $go, ".", stringify!($map), "`). A duplicate `",
                stringify!($key), "` is an error."
            )]
            pub async fn $map(&self, request: $list) -> Result<BTreeMap<String, String>> {
                let items = self.list_all(request).await?;
                lookup::unique_map(
                    &items,
                    stringify!($key),
                    |v: &$item| v.$key.clone().unwrap_or_default(),
                    |v: &$item| v.id.clone().unwrap_or_default(),
                )
            }

            #[doc = concat!(
                "The single ", $what, " whose `", stringify!($key), "` is `name` (Go: `", $go,
                ".", stringify!($get_by), "`), found with a SCIM `", $attr, " eq` filter. \
                 None, or more than one, is an error."
            )]
            pub async fn $get_by(&self, name: &str) -> Result<$item> {
                // Go lists everything; a SCIM filter asks the server for the
                // candidates, and the exact comparison below keeps Go's
                // result (SCIM `eq` may ignore case).
                let filter = format!("{} eq {}", $attr, scim_string(name));
                let items = self.list_all(<$list>::default().with_filter(filter)).await?;
                lookup::single(items, stringify!($item), name, |v: &$item| {
                    v.$key.clone().unwrap_or_default()
                })
            }
        }
    };
}

scim_helpers!(
    UsersV2Api,
    User,
    "user",
    get = GetUserRequest,
    delete = DeleteUserRequest,
    list = ListUsersRequest,
    key = user_name,
    attr = "userName",
    map = user_user_name_to_id_map,
    get_by = get_by_user_name,
    go = "UsersAPI"
);
scim_helpers!(
    GroupsV2Api,
    Group,
    "group",
    get = GetGroupRequest,
    delete = DeleteGroupRequest,
    list = ListGroupsRequest,
    key = display_name,
    attr = "displayName",
    map = group_display_name_to_id_map,
    get_by = get_by_display_name,
    go = "GroupsAPI"
);
scim_helpers!(
    ServicePrincipalsV2Api,
    ServicePrincipal,
    "service principal",
    get = GetServicePrincipalRequest,
    delete = DeleteServicePrincipalRequest,
    list = ListServicePrincipalsRequest,
    key = display_name,
    attr = "displayName",
    map = service_principal_display_name_to_id_map,
    get_by = get_by_display_name,
    go = "ServicePrincipalsAPI"
);
scim_helpers!(
    AccountUsersV2Api,
    AccountUser,
    "user",
    get = GetAccountUserRequest,
    delete = DeleteAccountUserRequest,
    list = ListAccountUsersRequest,
    key = user_name,
    attr = "userName",
    map = user_user_name_to_id_map,
    get_by = get_by_user_name,
    go = "AccountUsersAPI"
);
scim_helpers!(
    AccountGroupsV2Api,
    AccountGroup,
    "group",
    get = GetAccountGroupRequest,
    delete = DeleteAccountGroupRequest,
    list = ListAccountGroupsRequest,
    key = display_name,
    attr = "displayName",
    map = group_display_name_to_id_map,
    get_by = get_by_display_name,
    go = "AccountGroupsAPI"
);
scim_helpers!(
    AccountServicePrincipalsV2Api,
    AccountServicePrincipal,
    "service principal",
    get = GetAccountServicePrincipalRequest,
    delete = DeleteAccountServicePrincipalRequest,
    list = ListAccountServicePrincipalsRequest,
    key = display_name,
    attr = "displayName",
    map = service_principal_display_name_to_id_map,
    get_by = get_by_display_name,
    go = "AccountServicePrincipalsAPI"
);
