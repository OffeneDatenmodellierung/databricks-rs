//! Contract tests for the endpoints `dbk_tool` depends on (#7–#10, #12).
//!
//! Each test pins the wire contract of one API family: verb, path, query
//! string and the body keys that matter, plus response decoding. Every
//! mock must be hit exactly once (checked when the server drops), so a
//! generator or runtime change that alters any of these calls fails here.
//! See ADR-0001: generated shapes are covered by `generated_patterns.rs`;
//! these tests are the contract for the endpoints we rely on.

#![cfg(all(
    feature = "billing",
    feature = "catalog",
    feature = "compute",
    feature = "iam",
    feature = "serving",
    feature = "settings",
    feature = "sql",
    feature = "tags",
    feature = "workspace"
))]

use community_databricks_sdk::core::config::HostMetadata;
use community_databricks_sdk::service::billing::{
    BudgetPolicy, CreateBudgetPolicyRequest, DeleteBudgetPolicyRequest, GetBudgetPolicyRequest,
    ListBudgetPoliciesRequest, UpdateBudgetPolicyRequest,
};
use community_databricks_sdk::service::catalog::{
    CreateExternalLocation, CreateStorageCredential, DeleteExternalLocationRequest,
    DeleteStorageCredentialRequest, GetExternalLocationRequest, GetGrantRequest,
    GetStorageCredentialRequest, ListExternalLocationsRequest, ListPrivilegeAssignmentsRequest,
    ListStorageCredentialsRequest, PermissionsChange, Privilege, UpdateExternalLocation,
    UpdatePermissions, UpdateStorageCredential,
};
use community_databricks_sdk::service::compute::{
    CreateCluster, CreateInstancePool, CreatePolicy, DeleteCluster, DeleteInstancePool,
    DeletePolicy, EditCluster, EditInstancePool, EditPolicy, GetClusterPolicyRequest,
    GetClusterRequest, GetInstancePoolRequest, ListClusterPoliciesRequest, ListClustersRequest,
    UpdateCluster,
};
use community_databricks_sdk::service::iam::{
    CreateAccountGroupRequest, CreateAccountServicePrincipalRequest, CreateAccountUserRequest,
    CreateGroupRequest, CreateServicePrincipalRequest, CreateUserRequest,
    DeleteAccountGroupRequest, DeleteAccountServicePrincipalRequest, DeleteAccountUserRequest,
    DeleteGroupRequest, DeleteServicePrincipalRequest, DeleteUserRequest,
    DeleteWorkspaceAssignmentRequest, GetAccountGroupRequest, GetAccountServicePrincipalRequest,
    GetAccountUserRequest, GetGroupRequest, GetServicePrincipalRequest, GetUserRequest,
    GetWorkspaceAssignmentRequest, ListAccountGroupsRequest, ListAccountServicePrincipalsRequest,
    ListAccountUsersRequest, ListGroupsRequest, ListServicePrincipalsRequest, ListUsersRequest,
    ListWorkspaceAssignmentRequest, Patch, PatchAccountGroupRequest,
    PatchAccountServicePrincipalRequest, PatchAccountUserRequest, PatchGroupRequest, PatchOp,
    PatchServicePrincipalRequest, PatchUserRequest, UpdateAccountGroupRequest,
    UpdateAccountServicePrincipalRequest, UpdateAccountUserRequest, UpdateGroupRequest,
    UpdateServicePrincipalRequest, UpdateUserRequest, UpdateWorkspaceAssignments,
    WorkspacePermission,
};
use community_databricks_sdk::service::serving::{
    CreateServingEndpoint, DeleteServingEndpointRequest, EndpointCoreConfigInput, EndpointTag,
    GetServingEndpointRequest, PatchServingEndpointTags, ServedEntityInput,
};
use community_databricks_sdk::service::settings::{
    AccountNetworkPolicy, CreateNetworkPolicyRequest, CreateNotificationDestinationRequest,
    DeleteNetworkPolicyRequest, DeleteNotificationDestinationRequest, GetNetworkPolicyRequest,
    GetNotificationDestinationRequest, ListNetworkPoliciesRequest,
    ListNotificationDestinationsRequest, UpdateNetworkPolicyRequest,
    UpdateNotificationDestinationRequest,
};
use community_databricks_sdk::service::sql::{
    CreateWarehouseRequest, DeleteWarehouseRequest, EditWarehouseRequest, GetWarehouseRequest,
    ListWarehousesRequest,
};
use community_databricks_sdk::service::tags::{
    CreateTagPolicyRequest, DeleteTagPolicyRequest, GetTagPolicyRequest, ListTagPoliciesRequest,
    TagPolicy, UpdateTagPolicyRequest, Value as TagValue,
};
use community_databricks_sdk::service::workspace::{
    AclPermission, CreateScope, DeleteAcl, DeleteScope, GetAclRequest, ListAclsRequest, PutAcl,
};
use community_databricks_sdk::{AccountClient, Config, WorkspaceClient};
use futures_util::TryStreamExt;
use serde_json::{Value, json};
use wiremock::matchers::{body_partial_json, method, path, query_param};
use wiremock::{Mock, MockServer, ResponseTemplate};

const ACC: &str = "acc-1";

fn cfg(server: &MockServer) -> Config {
    let mut c = Config::with_host(server.uri()).token("dapi");
    c.host_metadata = Some(HostMetadata::default());
    c.rate_limit = Some(1000);
    c
}

async fn workspace(server: &MockServer) -> WorkspaceClient {
    WorkspaceClient::new(cfg(server)).await.unwrap()
}

async fn account(server: &MockServer) -> AccountClient {
    AccountClient::new(cfg(server).account(ACC)).await.unwrap()
}

/// One expected call. It must arrive exactly once with this verb, path,
/// query parameters and (a superset of) this JSON body.
struct Expect {
    verb: &'static str,
    path: String,
    query: Vec<(&'static str, String)>,
    body: Option<Value>,
    reply: Value,
}

fn call(verb: &'static str, path: impl Into<String>) -> Expect {
    Expect {
        verb,
        path: path.into(),
        query: Vec::new(),
        body: None,
        reply: json!({}),
    }
}

impl Expect {
    fn query(mut self, k: &'static str, v: impl Into<String>) -> Self {
        self.query.push((k, v.into()));
        self
    }

    fn body(mut self, b: Value) -> Self {
        self.body = Some(b);
        self
    }

    fn reply(mut self, r: Value) -> Self {
        self.reply = r;
        self
    }

    async fn mount(self, server: &MockServer) {
        let mut m = Mock::given(method(self.verb)).and(path(self.path.as_str()));
        for (k, v) in &self.query {
            m = m.and(query_param(*k, v.as_str()));
        }
        if let Some(b) = self.body {
            m = m.and(body_partial_json(b));
        }
        m.respond_with(ResponseTemplate::new(200).set_body_json(self.reply))
            .expect(1)
            .mount(server)
            .await;
    }
}

// ------------------------------------------------------- #7 Unity Catalog

#[tokio::test]
async fn uc_grants_get_update_and_paged_list() {
    let s = MockServer::start().await;
    let perms = "/api/2.1/unity-catalog/permissions/catalog/main";
    call("GET", perms)
        .query("principal", "data-eng")
        .reply(json!({"privilege_assignments": [
            {"principal": "data-eng", "privileges": ["USE_CATALOG", "SELECT"]}
        ]}))
        .mount(&s)
        .await;
    call("PATCH", perms)
        .body(json!({"changes": [
            {"principal": "data-eng", "add": ["MODIFY"], "remove": ["SELECT"]}
        ]}))
        .reply(json!({"privilege_assignments": []}))
        .mount(&s)
        .await;
    let list = "/api/2.1/unity-catalog/privilege-assignments/catalog/main";
    Mock::given(method("GET"))
        .and(path(list))
        .and(query_param("page_token", "p2"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(json!({"privilege_assignments": [{"principal": "b"}]})),
        )
        .expect(1)
        .mount(&s)
        .await;
    Mock::given(method("GET"))
        .and(path(list))
        .respond_with(ResponseTemplate::new(200).set_body_json(
            json!({"privilege_assignments": [{"principal": "a"}], "next_page_token": "p2"}),
        ))
        .up_to_n_times(1)
        .expect(1)
        .mount(&s)
        .await;

    let g = workspace(&s).await.grants();
    let got = g
        .get(GetGrantRequest::new("main", "catalog").with_principal("data-eng"))
        .await
        .unwrap();
    let a = &got.privilege_assignments[0];
    assert_eq!(a.privileges, [Privilege::UseCatalog, Privilege::Select]);
    g.update(UpdatePermissions::new("main", "catalog").with_changes(vec![
            PermissionsChange::default()
                .with_principal("data-eng")
                .with_add(vec![Privilege::Modify])
                .with_remove(vec![Privilege::Select]),
        ]))
    .await
    .unwrap();
    let all = g
        .list_all(ListPrivilegeAssignmentsRequest::new("main", "catalog"))
        .await
        .unwrap();
    let names: Vec<_> = all.iter().filter_map(|a| a.principal.clone()).collect();
    assert_eq!(names, ["a", "b"]);
}

#[tokio::test]
async fn uc_external_locations_crud() {
    let s = MockServer::start().await;
    let base = "/api/2.1/unity-catalog/external-locations";
    let info = json!({"name": "raw", "url": "s3://bucket/raw", "credential_name": "cred"});
    call("POST", base)
        .body(json!({"name": "raw", "url": "s3://bucket/raw", "credential_name": "cred"}))
        .reply(info.clone())
        .mount(&s)
        .await;
    call("GET", format!("{base}/raw"))
        .reply(info.clone())
        .mount(&s)
        .await;
    call("GET", base)
        .query("max_results", "50")
        .reply(json!({"external_locations": [info.clone()]}))
        .mount(&s)
        .await;
    call("PATCH", format!("{base}/raw"))
        .body(json!({"comment": "landing zone", "read_only": true}))
        .reply(info.clone())
        .mount(&s)
        .await;
    call("DELETE", format!("{base}/raw"))
        .query("force", "true")
        .mount(&s)
        .await;

    let e = workspace(&s).await.external_locations();
    let created = e
        .create(
            CreateExternalLocation::default()
                .with_name("raw")
                .with_url("s3://bucket/raw")
                .with_credential_name("cred"),
        )
        .await
        .unwrap();
    assert_eq!(created.url.as_deref(), Some("s3://bucket/raw"));
    e.get(GetExternalLocationRequest::new("raw")).await.unwrap();
    let all = e
        .list_all(ListExternalLocationsRequest::default().with_max_results(50))
        .await
        .unwrap();
    assert_eq!(all.len(), 1);
    e.update(
        UpdateExternalLocation::new("raw")
            .with_comment("landing zone")
            .with_read_only(true),
    )
    .await
    .unwrap();
    e.delete(DeleteExternalLocationRequest::new("raw").with_force(true))
        .await
        .unwrap();
}

#[tokio::test]
async fn uc_storage_credentials_crud() {
    let s = MockServer::start().await;
    let base = "/api/2.1/unity-catalog/storage-credentials";
    let info = json!({"name": "cred", "aws_iam_role": {"role_arn": "arn:aws:iam::1:role/uc"}});
    call("POST", base)
        .body(json!({"name": "cred", "comment": "uc"}))
        .reply(info.clone())
        .mount(&s)
        .await;
    call("GET", format!("{base}/cred"))
        .reply(info.clone())
        .mount(&s)
        .await;
    // UC lists page only when max_results is sent; unset means 0 (the
    // server's page size), as in Go.
    call("GET", base)
        .query("max_results", "0")
        .reply(json!({"storage_credentials": [info.clone()]}))
        .mount(&s)
        .await;
    call("PATCH", format!("{base}/cred"))
        .body(json!({"new_name": "cred2"}))
        .reply(info.clone())
        .mount(&s)
        .await;
    call("DELETE", format!("{base}/cred")).mount(&s).await;

    let c = workspace(&s).await.storage_credentials();
    let created = c
        .create(CreateStorageCredential::new("cred").with_comment("uc"))
        .await
        .unwrap();
    assert_eq!(
        created.aws_iam_role.map(|r| r.role_arn).as_deref(),
        Some("arn:aws:iam::1:role/uc")
    );
    c.get(GetStorageCredentialRequest::new("cred"))
        .await
        .unwrap();
    assert_eq!(
        c.list_all(ListStorageCredentialsRequest::default())
            .await
            .unwrap()
            .len(),
        1
    );
    c.update(UpdateStorageCredential::new("cred").with_new_name("cred2"))
        .await
        .unwrap();
    c.delete(DeleteStorageCredentialRequest::new("cred"))
        .await
        .unwrap();
}

// ------------------------------------------------------------ #8 SCIM

/// Mounts get / list (two SCIM pages) / create / update / patch / delete
/// for one SCIM resource under `base`.
async fn mount_scim(s: &MockServer, base: &str, item: &Value, create_body: Value) {
    call("POST", base)
        .body(create_body)
        .reply(item.clone())
        .mount(s)
        .await;
    call("GET", format!("{base}/1"))
        .reply(item.clone())
        .mount(s)
        .await;
    call("PUT", format!("{base}/1")).mount(s).await;
    call("PATCH", format!("{base}/1"))
        .body(json!({"Operations": [{"op": "replace", "path": "active", "value": false}]}))
        .mount(s)
        .await;
    call("DELETE", format!("{base}/1")).mount(s).await;
    // SCIM pagination (as Go): start at 1, advance by the items seen, stop
    // on an empty page.
    for (start, items) in [
        (1, vec![item.clone()]),
        (2, vec![item.clone()]),
        (3, vec![]),
    ] {
        call("GET", base)
            .query("startIndex", start.to_string())
            .query("count", "1")
            .reply(json!({"totalResults": 2, "startIndex": start, "Resources": items}))
            .mount(s)
            .await;
    }
}

fn deactivate() -> Vec<Patch> {
    vec![
        Patch::default()
            .with_op(PatchOp::Replace)
            .with_path("active")
            .with_value(json!(false)),
    ]
}

#[tokio::test]
async fn scim_workspace_users_groups_service_principals() {
    let server = MockServer::start().await;
    let scim = "/api/2.0/preview/scim/v2";
    let user = json!({"id": "1", "userName": "a@example.com", "active": true});
    let group = json!({"id": "1", "displayName": "data-eng"});
    let sp = json!({"id": "1", "applicationId": "app-1", "displayName": "bot"});
    mount_scim(
        &server,
        &format!("{scim}/Users"),
        &user,
        json!({"userName": "a@example.com"}),
    )
    .await;
    mount_scim(
        &server,
        &format!("{scim}/Groups"),
        &group,
        json!({"displayName": "data-eng"}),
    )
    .await;
    mount_scim(
        &server,
        &format!("{scim}/ServicePrincipals"),
        &sp,
        json!({"applicationId": "app-1"}),
    )
    .await;
    let w = workspace(&server).await;

    let u = w.users_v2();
    let created = u
        .create(CreateUserRequest::default().with_user_name("a@example.com"))
        .await
        .unwrap();
    assert_eq!(created.user_name.as_deref(), Some("a@example.com"));
    u.get(GetUserRequest::new("1")).await.unwrap();
    let all = u
        .list_all(ListUsersRequest::default().with_count(1))
        .await
        .unwrap();
    assert_eq!(all.len(), 2);
    u.update(UpdateUserRequest::new("1")).await.unwrap();
    u.patch(PatchUserRequest::new("1").with_operations(deactivate()))
        .await
        .unwrap();
    u.delete(DeleteUserRequest::new("1")).await.unwrap();

    let g = w.groups_v2();
    g.create(CreateGroupRequest::default().with_display_name("data-eng"))
        .await
        .unwrap();
    g.get(GetGroupRequest::new("1")).await.unwrap();
    assert_eq!(
        g.list_all(ListGroupsRequest::default().with_count(1))
            .await
            .unwrap()
            .len(),
        2
    );
    g.update(UpdateGroupRequest::new("1")).await.unwrap();
    g.patch(PatchGroupRequest::new("1").with_operations(deactivate()))
        .await
        .unwrap();
    g.delete(DeleteGroupRequest::new("1")).await.unwrap();

    let p = w.service_principals_v2();
    p.create(CreateServicePrincipalRequest::default().with_application_id("app-1"))
        .await
        .unwrap();
    p.get(GetServicePrincipalRequest::new("1")).await.unwrap();
    assert_eq!(
        p.list_all(ListServicePrincipalsRequest::default().with_count(1))
            .await
            .unwrap()
            .len(),
        2
    );
    p.update(UpdateServicePrincipalRequest::new("1"))
        .await
        .unwrap();
    p.patch(PatchServicePrincipalRequest::new("1").with_operations(deactivate()))
        .await
        .unwrap();
    p.delete(DeleteServicePrincipalRequest::new("1"))
        .await
        .unwrap();
}

#[tokio::test]
async fn scim_account_users_groups_service_principals() {
    let server = MockServer::start().await;
    let scim = format!("/api/2.0/accounts/{ACC}/scim/v2");
    let user = json!({"id": "1", "userName": "a@example.com"});
    let group = json!({"id": "1", "displayName": "admins"});
    let sp = json!({"id": "1", "applicationId": "app-1"});
    mount_scim(
        &server,
        &format!("{scim}/Users"),
        &user,
        json!({"userName": "a@example.com"}),
    )
    .await;
    mount_scim(
        &server,
        &format!("{scim}/Groups"),
        &group,
        json!({"displayName": "admins"}),
    )
    .await;
    mount_scim(
        &server,
        &format!("{scim}/ServicePrincipals"),
        &sp,
        json!({"applicationId": "app-1"}),
    )
    .await;
    let a = account(&server).await;

    let u = a.users_v2();
    u.create(CreateAccountUserRequest::default().with_user_name("a@example.com"))
        .await
        .unwrap();
    u.get(GetAccountUserRequest::new("1")).await.unwrap();
    assert_eq!(
        u.list_all(ListAccountUsersRequest::default().with_count(1))
            .await
            .unwrap()
            .len(),
        2
    );
    u.update(UpdateAccountUserRequest::new("1")).await.unwrap();
    u.patch(PatchAccountUserRequest::new("1").with_operations(deactivate()))
        .await
        .unwrap();
    u.delete(DeleteAccountUserRequest::new("1")).await.unwrap();

    let g = a.groups_v2();
    g.create(CreateAccountGroupRequest::default().with_display_name("admins"))
        .await
        .unwrap();
    g.get(GetAccountGroupRequest::new("1")).await.unwrap();
    assert_eq!(
        g.list_all(ListAccountGroupsRequest::default().with_count(1))
            .await
            .unwrap()
            .len(),
        2
    );
    g.update(UpdateAccountGroupRequest::new("1")).await.unwrap();
    g.patch(PatchAccountGroupRequest::new("1").with_operations(deactivate()))
        .await
        .unwrap();
    g.delete(DeleteAccountGroupRequest::new("1")).await.unwrap();

    let p = a.service_principals_v2();
    p.create(CreateAccountServicePrincipalRequest::default().with_application_id("app-1"))
        .await
        .unwrap();
    p.get(GetAccountServicePrincipalRequest::new("1"))
        .await
        .unwrap();
    assert_eq!(
        p.list_all(ListAccountServicePrincipalsRequest::default().with_count(1))
            .await
            .unwrap()
            .len(),
        2
    );
    p.update(UpdateAccountServicePrincipalRequest::new("1"))
        .await
        .unwrap();
    p.patch(PatchAccountServicePrincipalRequest::new("1").with_operations(deactivate()))
        .await
        .unwrap();
    p.delete(DeleteAccountServicePrincipalRequest::new("1"))
        .await
        .unwrap();
}

#[tokio::test]
async fn workspace_assignment() {
    let s = MockServer::start().await;
    let base = format!("/api/2.0/accounts/{ACC}/workspaces/42/permissionassignments");
    call("PUT", format!("{base}/principals/7"))
        .body(json!({"permissions": ["USER"]}))
        .reply(json!({"principal": {"principal_id": 7}, "permissions": ["USER"]}))
        .mount(&s)
        .await;
    call("GET", base.clone())
        .reply(json!({"permission_assignments": [{"principal": {"principal_id": 7}, "permissions": ["USER"]}]}))
        .mount(&s)
        .await;
    call("GET", format!("{base}/permissions"))
        .reply(json!({"permissions": [{"permission_level": "USER"}]}))
        .mount(&s)
        .await;
    call("DELETE", format!("{base}/principals/7"))
        .mount(&s)
        .await;

    let w = account(&s).await.workspace_assignment();
    let assigned = w
        .update(
            UpdateWorkspaceAssignments::new(7, 42)
                .with_permissions(vec![WorkspacePermission::User]),
        )
        .await
        .unwrap();
    assert_eq!(assigned.permissions, [WorkspacePermission::User]);
    assert_eq!(
        w.list_all(ListWorkspaceAssignmentRequest::new(42))
            .await
            .unwrap()
            .len(),
        1
    );
    w.get(GetWorkspaceAssignmentRequest::new(42)).await.unwrap();
    w.delete(DeleteWorkspaceAssignmentRequest::new(7, 42))
        .await
        .unwrap();
}

// ------------------------------------------------------------ #9 compute

#[tokio::test]
async fn clusters_create_edit_update_delete() {
    let s = MockServer::start().await;
    let c = "/api/2.1/clusters";
    call("POST", format!("{c}/create"))
        .body(json!({"spark_version": "16.4.x-scala2.12", "cluster_name": "etl", "num_workers": 2}))
        .reply(json!({"cluster_id": "c-1"}))
        .mount(&s)
        .await;
    call("POST", format!("{c}/edit"))
        .body(json!({"cluster_id": "c-1", "spark_version": "16.4.x-scala2.12", "num_workers": 4}))
        .mount(&s)
        .await;
    call("POST", format!("{c}/update"))
        .body(json!({"cluster_id": "c-1", "update_mask": "autotermination_minutes"}))
        .mount(&s)
        .await;
    call("GET", format!("{c}/get"))
        .query("cluster_id", "c-1")
        .reply(json!({"cluster_id": "c-1", "state": "RUNNING"}))
        .mount(&s)
        .await;
    call("GET", format!("{c}/list"))
        .query("page_size", "10")
        .reply(json!({"clusters": [{"cluster_id": "c-1"}]}))
        .mount(&s)
        .await;
    call("POST", format!("{c}/delete"))
        .body(json!({"cluster_id": "c-1"}))
        .mount(&s)
        .await;

    let cl = workspace(&s).await.clusters();
    let created = cl
        .create(
            CreateCluster::new("16.4.x-scala2.12")
                .with_cluster_name("etl")
                .with_num_workers(2),
        )
        .await
        .unwrap();
    assert_eq!(created.response.cluster_id.as_deref(), Some("c-1"));
    cl.edit(EditCluster::new("c-1", "16.4.x-scala2.12").with_num_workers(4))
        .await
        .unwrap();
    cl.update(UpdateCluster::new("c-1", "autotermination_minutes"))
        .await
        .unwrap();
    cl.get(GetClusterRequest::new("c-1")).await.unwrap();
    let listed: Vec<_> = cl
        .list(ListClustersRequest::default().with_page_size(10))
        .try_collect()
        .await
        .unwrap();
    assert_eq!(listed.len(), 1);
    cl.delete(DeleteCluster::new("c-1")).await.unwrap();
}

#[tokio::test]
async fn cluster_policies_and_instance_pools_crud() {
    let s = MockServer::start().await;
    let p = "/api/2.0/policies/clusters";
    call("POST", format!("{p}/create"))
        .body(json!({"name": "small", "definition": "{}"}))
        .reply(json!({"policy_id": "p-1"}))
        .mount(&s)
        .await;
    call("GET", format!("{p}/get"))
        .query("policy_id", "p-1")
        .reply(json!({"policy_id": "p-1", "name": "small"}))
        .mount(&s)
        .await;
    call("GET", format!("{p}/list"))
        .reply(json!({"policies": [{"policy_id": "p-1"}]}))
        .mount(&s)
        .await;
    call("POST", format!("{p}/edit"))
        .body(json!({"policy_id": "p-1", "max_clusters_per_user": 2}))
        .mount(&s)
        .await;
    call("POST", format!("{p}/delete"))
        .body(json!({"policy_id": "p-1"}))
        .mount(&s)
        .await;
    let ip = "/api/2.0/instance-pools";
    call("POST", format!("{ip}/create"))
        .body(json!({"instance_pool_name": "pool", "node_type_id": "i3.xlarge"}))
        .reply(json!({"instance_pool_id": "ip-1"}))
        .mount(&s)
        .await;
    call("GET", format!("{ip}/get"))
        .query("instance_pool_id", "ip-1")
        .reply(json!({"instance_pool_id": "ip-1"}))
        .mount(&s)
        .await;
    call("GET", format!("{ip}/list"))
        .reply(json!({"instance_pools": [{"instance_pool_id": "ip-1"}]}))
        .mount(&s)
        .await;
    call("POST", format!("{ip}/edit"))
        .body(json!({"instance_pool_id": "ip-1", "max_capacity": 10}))
        .mount(&s)
        .await;
    call("POST", format!("{ip}/delete"))
        .body(json!({"instance_pool_id": "ip-1"}))
        .mount(&s)
        .await;

    let w = workspace(&s).await;
    let pol = w.cluster_policies();
    let id = pol
        .create(
            CreatePolicy::default()
                .with_name("small")
                .with_definition("{}"),
        )
        .await
        .unwrap()
        .policy_id;
    assert_eq!(id.as_deref(), Some("p-1"));
    pol.get(GetClusterPolicyRequest::new("p-1")).await.unwrap();
    assert_eq!(
        pol.list_all(ListClusterPoliciesRequest::default())
            .await
            .unwrap()
            .len(),
        1
    );
    pol.edit(EditPolicy::new("p-1").with_max_clusters_per_user(2))
        .await
        .unwrap();
    pol.delete(DeletePolicy::new("p-1")).await.unwrap();

    let pools = w.instance_pools();
    let id = pools
        .create(CreateInstancePool::new("pool", "i3.xlarge"))
        .await
        .unwrap()
        .instance_pool_id;
    assert_eq!(id.as_deref(), Some("ip-1"));
    pools
        .get(GetInstancePoolRequest::new("ip-1"))
        .await
        .unwrap();
    assert_eq!(pools.list_all().await.unwrap().len(), 1);
    pools
        .edit(
            EditInstancePool::default()
                .with_instance_pool_id("ip-1")
                .with_max_capacity(10),
        )
        .await
        .unwrap();
    pools.delete(DeleteInstancePool::new("ip-1")).await.unwrap();
}

// ------------------------------------------------------------- #10 tail

#[tokio::test]
async fn sql_warehouses_crud() {
    let s = MockServer::start().await;
    let base = "/api/2.0/sql/warehouses";
    call("POST", base)
        .body(json!({"name": "bi", "cluster_size": "Small", "auto_stop_mins": 10}))
        .reply(json!({"id": "wh-1"}))
        .mount(&s)
        .await;
    call("GET", format!("{base}/wh-1"))
        .reply(json!({"id": "wh-1", "state": "RUNNING"}))
        .mount(&s)
        .await;
    call("GET", base)
        .reply(json!({"warehouses": [{"id": "wh-1"}]}))
        .mount(&s)
        .await;
    call("POST", format!("{base}/wh-1/edit"))
        .body(json!({"max_num_clusters": 3}))
        .mount(&s)
        .await;
    call("DELETE", format!("{base}/wh-1")).mount(&s).await;

    let wh = workspace(&s).await.warehouses();
    let created = wh
        .create(
            CreateWarehouseRequest::default()
                .with_name("bi")
                .with_cluster_size("Small")
                .with_auto_stop_mins(10),
        )
        .await
        .unwrap();
    assert_eq!(created.response.id.as_deref(), Some("wh-1"));
    wh.get(GetWarehouseRequest::new("wh-1")).await.unwrap();
    assert_eq!(
        wh.list_all(ListWarehousesRequest::default())
            .await
            .unwrap()
            .len(),
        1
    );
    wh.edit(EditWarehouseRequest::new("wh-1").with_max_num_clusters(3))
        .await
        .unwrap();
    wh.delete(DeleteWarehouseRequest::new("wh-1"))
        .await
        .unwrap();
}

#[tokio::test]
async fn secret_scopes_and_acls() {
    let s = MockServer::start().await;
    let sec = "/api/2.0/secrets";
    call("POST", format!("{sec}/scopes/create"))
        .body(json!({"scope": "app"}))
        .mount(&s)
        .await;
    call("GET", format!("{sec}/scopes/list"))
        .reply(json!({"scopes": [{"name": "app", "backend_type": "DATABRICKS"}]}))
        .mount(&s)
        .await;
    call("POST", format!("{sec}/acls/put"))
        .body(json!({"scope": "app", "principal": "data-eng", "permission": "READ"}))
        .mount(&s)
        .await;
    call("GET", format!("{sec}/acls/get"))
        .query("scope", "app")
        .query("principal", "data-eng")
        .reply(json!({"principal": "data-eng", "permission": "READ"}))
        .mount(&s)
        .await;
    call("GET", format!("{sec}/acls/list"))
        .query("scope", "app")
        .reply(json!({"items": [{"principal": "data-eng", "permission": "READ"}]}))
        .mount(&s)
        .await;
    call("POST", format!("{sec}/acls/delete"))
        .body(json!({"scope": "app", "principal": "data-eng"}))
        .mount(&s)
        .await;
    call("POST", format!("{sec}/scopes/delete"))
        .body(json!({"scope": "app"}))
        .mount(&s)
        .await;

    let sc = workspace(&s).await.secrets();
    sc.create_scope(CreateScope::new("app")).await.unwrap();
    let scopes: Vec<_> = sc.list_scopes().try_collect().await.unwrap();
    assert_eq!(scopes[0].name.as_deref(), Some("app"));
    sc.put_acl(
        PutAcl::default()
            .with_scope("app")
            .with_principal("data-eng")
            .with_permission(AclPermission::Read),
    )
    .await
    .unwrap();
    let acl = sc
        .get_acl(GetAclRequest::new("data-eng", "app"))
        .await
        .unwrap();
    assert_eq!(acl.permission, AclPermission::Read);
    let acls: Vec<_> = sc
        .list_acls(ListAclsRequest::new("app"))
        .try_collect()
        .await
        .unwrap();
    assert_eq!(acls.len(), 1);
    sc.delete_acl(DeleteAcl::new("data-eng", "app"))
        .await
        .unwrap();
    sc.delete_scope(DeleteScope::new("app")).await.unwrap();
}

#[tokio::test]
async fn serving_endpoints_crud() {
    let s = MockServer::start().await;
    let base = "/api/2.0/serving-endpoints";
    let detail =
        json!({"name": "chat", "state": {"config_update": "NOT_UPDATING", "ready": "READY"}});
    call("POST", base)
        .body(json!({"name": "chat"}))
        .reply(detail.clone())
        .mount(&s)
        .await;
    call("GET", format!("{base}/chat"))
        .reply(detail.clone())
        .mount(&s)
        .await;
    call("GET", base)
        .reply(json!({"endpoints": [{"name": "chat"}]}))
        .mount(&s)
        .await;
    call("PUT", format!("{base}/chat/config"))
        .body(json!({"served_entities": [{"entity_name": "m", "entity_version": "2"}]}))
        .reply(detail.clone())
        .mount(&s)
        .await;
    call("PATCH", format!("{base}/chat/tags"))
        .body(json!({"add_tags": [{"key": "team", "value": "ml"}]}))
        .reply(json!({"tags": [{"key": "team", "value": "ml"}]}))
        .mount(&s)
        .await;
    call("DELETE", format!("{base}/chat")).mount(&s).await;

    let se = workspace(&s).await.serving_endpoints();
    let created = se.create(CreateServingEndpoint::new("chat")).await.unwrap();
    assert_eq!(created.response.name.as_deref(), Some("chat"));
    se.get(GetServingEndpointRequest::new("chat"))
        .await
        .unwrap();
    assert_eq!(se.list_all().await.unwrap().len(), 1);
    se.update_config(
        EndpointCoreConfigInput::new("chat").with_served_entities(vec![
            ServedEntityInput::default()
                .with_entity_name("m")
                .with_entity_version("2"),
        ]),
    )
    .await
    .unwrap();
    let tags = se
        .patch(
            PatchServingEndpointTags::new("chat")
                .with_add_tags(vec![EndpointTag::new("team").with_value("ml")]),
        )
        .await
        .unwrap();
    assert_eq!(tags.tags.len(), 1);
    se.delete(DeleteServingEndpointRequest::new("chat"))
        .await
        .unwrap();
}

#[tokio::test]
async fn account_network_policies_crud() {
    let s = MockServer::start().await;
    let base = format!("/api/2.0/accounts/{ACC}/network-policies");
    let policy = json!({"network_policy_id": "np-1", "egress": {"network_access": {"restriction_mode": "RESTRICTED_ACCESS"}}});
    call("POST", base.clone())
        .body(json!({"network_policy_id": "np-1"}))
        .reply(policy.clone())
        .mount(&s)
        .await;
    call("GET", format!("{base}/np-1"))
        .reply(policy.clone())
        .mount(&s)
        .await;
    call("GET", base.clone())
        .reply(json!({"items": [policy.clone()]}))
        .mount(&s)
        .await;
    call("PUT", format!("{base}/np-1"))
        .body(json!({"network_policy_id": "np-1"}))
        .reply(policy.clone())
        .mount(&s)
        .await;
    call("DELETE", format!("{base}/np-1")).mount(&s).await;

    let np = account(&s).await.network_policies();
    let body = AccountNetworkPolicy::default().with_network_policy_id("np-1");
    let created = np
        .create_network_policy_rpc(CreateNetworkPolicyRequest::new(body.clone()))
        .await
        .unwrap();
    assert!(created.egress.is_some());
    np.get_network_policy_rpc(GetNetworkPolicyRequest::new("np-1"))
        .await
        .unwrap();
    assert_eq!(
        np.list_network_policies_rpc_all(ListNetworkPoliciesRequest::default())
            .await
            .unwrap()
            .len(),
        1
    );
    np.update_network_policy_rpc(UpdateNetworkPolicyRequest::new(body, "np-1"))
        .await
        .unwrap();
    np.delete_network_policy_rpc(DeleteNetworkPolicyRequest::new("np-1"))
        .await
        .unwrap();
}

#[tokio::test]
async fn notification_destinations_crud() {
    let s = MockServer::start().await;
    let base = "/api/2.0/notification-destinations";
    let dest = json!({"id": "nd-1", "display_name": "oncall", "destination_type": "SLACK"});
    call("POST", base)
        .body(json!({"display_name": "oncall"}))
        .reply(dest.clone())
        .mount(&s)
        .await;
    call("GET", format!("{base}/nd-1"))
        .reply(dest.clone())
        .mount(&s)
        .await;
    call("GET", base)
        .reply(json!({"results": [{"id": "nd-1"}]}))
        .mount(&s)
        .await;
    call("PATCH", format!("{base}/nd-1"))
        .body(json!({"display_name": "oncall-2"}))
        .reply(dest.clone())
        .mount(&s)
        .await;
    call("DELETE", format!("{base}/nd-1")).mount(&s).await;

    let nd = workspace(&s).await.notification_destinations();
    nd.create(CreateNotificationDestinationRequest::default().with_display_name("oncall"))
        .await
        .unwrap();
    nd.get(GetNotificationDestinationRequest::new("nd-1"))
        .await
        .unwrap();
    assert_eq!(
        nd.list_all(ListNotificationDestinationsRequest::default())
            .await
            .unwrap()
            .len(),
        1
    );
    nd.update(UpdateNotificationDestinationRequest::new("nd-1").with_display_name("oncall-2"))
        .await
        .unwrap();
    nd.delete(DeleteNotificationDestinationRequest::new("nd-1"))
        .await
        .unwrap();
}

#[tokio::test]
async fn account_budget_policies_crud_with_request_id() {
    let s = MockServer::start().await;
    let base = format!("/api/2.1/accounts/{ACC}/budget-policies");
    let policy = json!({"policy_id": "bp-1", "policy_name": "team-a"});
    call("POST", base.clone())
        .body(json!({"policy": {"policy_name": "team-a"}}))
        .reply(policy.clone())
        .mount(&s)
        .await;
    call("GET", format!("{base}/bp-1"))
        .reply(policy.clone())
        .mount(&s)
        .await;
    call("GET", base.clone())
        .reply(json!({"policies": [policy.clone()]}))
        .mount(&s)
        .await;
    call("PATCH", format!("{base}/bp-1"))
        .body(json!({"policy_name": "team-b"}))
        .reply(policy.clone())
        .mount(&s)
        .await;
    call("DELETE", format!("{base}/bp-1")).mount(&s).await;

    let bp = account(&s).await.budget_policy();
    bp.create(
        CreateBudgetPolicyRequest::default()
            .with_policy(BudgetPolicy::default().with_policy_name("team-a")),
    )
    .await
    .unwrap();
    // #3: the SDK filled in a request_id so the create is safe to retry.
    let create = &s.received_requests().await.unwrap()[0];
    let sent: Value = serde_json::from_slice(&create.body).unwrap();
    assert_eq!(sent["request_id"].as_str().map(str::len), Some(36));
    bp.get(GetBudgetPolicyRequest::new("bp-1")).await.unwrap();
    assert_eq!(
        bp.list_all(ListBudgetPoliciesRequest::default())
            .await
            .unwrap()
            .len(),
        1
    );
    bp.update(UpdateBudgetPolicyRequest::new(
        BudgetPolicy::default().with_policy_name("team-b"),
        "bp-1",
    ))
    .await
    .unwrap();
    bp.delete(DeleteBudgetPolicyRequest::new("bp-1"))
        .await
        .unwrap();
}

// ------------------------------------------------------ #12 tag policies

#[tokio::test]
async fn tag_policies_crud_and_paged_list() {
    let s = MockServer::start().await;
    let base = "/api/2.1/tag-policies";
    let policy = json!({"tag_key": "env", "values": [{"name": "prod"}, {"name": "dev"}]});
    call("POST", base)
        .body(json!({"tag_key": "env", "values": [{"name": "prod"}, {"name": "dev"}]}))
        .reply(policy.clone())
        .mount(&s)
        .await;
    call("GET", format!("{base}/env"))
        .reply(policy.clone())
        .mount(&s)
        .await;
    call("PATCH", format!("{base}/env"))
        .query("update_mask", "description,values")
        .body(json!({"description": "deployment stage"}))
        .reply(policy.clone())
        .mount(&s)
        .await;
    call("DELETE", format!("{base}/env")).mount(&s).await;
    call("GET", base)
        .query("page_size", "1")
        .query("page_token", "t2")
        .reply(json!({"tag_policies": [{"tag_key": "team"}]}))
        .mount(&s)
        .await;
    Mock::given(method("GET"))
        .and(path(base))
        .and(query_param("page_size", "1"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(
                json!({"tag_policies": [{"tag_key": "env"}], "next_page_token": "t2"}),
            ),
        )
        .up_to_n_times(1)
        .expect(1)
        .mount(&s)
        .await;

    let tp = workspace(&s).await.tag_policies();
    let values = vec![TagValue::new("prod"), TagValue::new("dev")];
    let created = tp
        .create_tag_policy(CreateTagPolicyRequest::new(
            TagPolicy::new("env").with_values(values),
        ))
        .await
        .unwrap();
    assert_eq!(created.values.len(), 2);
    let got = tp
        .get_tag_policy(GetTagPolicyRequest::new("env"))
        .await
        .unwrap();
    tp.update_tag_policy(
        UpdateTagPolicyRequest::default()
            .with_tag_key("env")
            .with_update_mask("description,values")
            .with_tag_policy(got.with_description("deployment stage")),
    )
    .await
    .unwrap();
    let keys: Vec<String> = tp
        .list_tag_policies_all(ListTagPoliciesRequest::default().with_page_size(1))
        .await
        .unwrap()
        .into_iter()
        .map(|p| p.tag_key)
        .collect();
    assert_eq!(keys, ["env", "team"]);
    tp.delete_tag_policy(DeleteTagPolicyRequest::new("env"))
        .await
        .unwrap();
}
