//! `Workspace.AzureResourceId` from Go's `service/provisioning/ext_azure.go`.

use crate::Workspace;

impl Workspace {
    /// The Azure resource ID of an Azure workspace
    /// (`/subscriptions/{sub}/resourceGroups/{rg}/providers/Microsoft.Databricks/workspaces/{name}`),
    /// or `None` for a workspace on another cloud or without those fields.
    #[must_use]
    pub fn azure_resource_id(&self) -> Option<String> {
        let azure = self.azure_workspace_info.as_ref()?;
        Some(format!(
            "/subscriptions/{}/resourceGroups/{}/providers/Microsoft.Databricks/workspaces/{}",
            azure.subscription_id.as_deref()?,
            azure.resource_group.as_deref()?,
            self.workspace_name.as_deref()?
        ))
    }
}
