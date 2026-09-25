//! Databricks deployment environments (Go: `common/environment`).

/// The cloud a Databricks deployment runs on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum Cloud {
    /// Amazon Web Services.
    Aws,
    /// Microsoft Azure.
    Azure,
    /// Google Cloud.
    Gcp,
}

impl Cloud {
    /// Parse `AWS`, `AZURE` or `GCP` (case-insensitive).
    #[must_use]
    pub fn parse(s: &str) -> Option<Self> {
        match s.to_ascii_uppercase().as_str() {
            "AWS" => Some(Self::Aws),
            "AZURE" => Some(Self::Azure),
            "GCP" => Some(Self::Gcp),
            _ => None,
        }
    }
}

/// Azure cloud endpoints.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub struct AzureEnvironment {
    /// `PUBLIC`, `USGOVERNMENT` or `CHINA`.
    pub name: &'static str,
    /// Resource for the service-management token.
    pub service_management_endpoint: &'static str,
    /// Azure Resource Manager.
    pub resource_manager_endpoint: &'static str,
    /// Microsoft Entra ID (AAD) authority.
    pub active_directory_endpoint: &'static str,
}

const AZURE_PUBLIC: AzureEnvironment = AzureEnvironment {
    name: "PUBLIC",
    service_management_endpoint: "https://management.core.windows.net/",
    resource_manager_endpoint: "https://management.azure.com/",
    active_directory_endpoint: "https://login.microsoftonline.com/",
};
const AZURE_US_GOV: AzureEnvironment = AzureEnvironment {
    name: "USGOVERNMENT",
    service_management_endpoint: "https://management.core.usgovcloudapi.net/",
    resource_manager_endpoint: "https://management.usgovcloudapi.net/",
    active_directory_endpoint: "https://login.microsoftonline.us/",
};
const AZURE_CHINA: AzureEnvironment = AzureEnvironment {
    name: "CHINA",
    service_management_endpoint: "https://management.core.chinacloudapi.cn/",
    resource_manager_endpoint: "https://management.chinacloudapi.cn/",
    active_directory_endpoint: "https://login.chinacloudapi.cn/",
};

/// A Databricks deployment: cloud, DNS zone and, on Azure, the Databricks
/// application ID and cloud endpoints.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub struct Environment {
    /// Cloud provider.
    pub cloud: Cloud,
    /// Host suffix, e.g. `.azuredatabricks.net`.
    pub dns_zone: &'static str,
    /// Azure Databricks application ID (the token resource); empty elsewhere.
    pub azure_application_id: &'static str,
    /// Azure endpoints; `None` elsewhere.
    pub azure: Option<AzureEnvironment>,
}

const AZURE_APP: &str = "2ff814a6-3304-4ab8-85cb-cd0e6f879c1d";

const fn env(
    cloud: Cloud,
    dns_zone: &'static str,
    azure_application_id: &'static str,
    azure: Option<AzureEnvironment>,
) -> Environment {
    Environment {
        cloud,
        dns_zone,
        azure_application_id,
        azure,
    }
}

const DEFAULT: Environment = env(Cloud::Aws, ".cloud.databricks.com", "", None);

// Order matters: the first matching suffix wins (as in Go).
const ALL: &[Environment] = &[
    env(Cloud::Aws, ".dev.databricks.com", "", None),
    env(Cloud::Aws, ".staging.cloud.databricks.com", "", None),
    env(Cloud::Aws, ".cloud.databricks.us", "", None),
    DEFAULT,
    env(
        Cloud::Azure,
        ".dev.azuredatabricks.net",
        "62a912ac-b58e-4c1d-89ea-b2dbfc7358fc",
        Some(AZURE_PUBLIC),
    ),
    env(
        Cloud::Azure,
        ".staging.azuredatabricks.net",
        "4a67d088-db5c-48f1-9ff2-0aace800ae68",
        Some(AZURE_PUBLIC),
    ),
    env(
        Cloud::Azure,
        ".azuredatabricks.net",
        AZURE_APP,
        Some(AZURE_PUBLIC),
    ),
    env(
        Cloud::Azure,
        ".databricks.azure.us",
        AZURE_APP,
        Some(AZURE_US_GOV),
    ),
    env(
        Cloud::Azure,
        ".databricks.azure.cn",
        AZURE_APP,
        Some(AZURE_CHINA),
    ),
    env(Cloud::Gcp, ".dev.gcp.databricks.com", "", None),
    env(Cloud::Gcp, ".staging.gcp.databricks.com", "", None),
    env(Cloud::Gcp, ".gcp.databricks.com", "", None),
];

pub(crate) fn for_hostname(hostname: &str) -> Environment {
    ALL.iter()
        .find(|e| hostname.ends_with(e.dns_zone))
        .copied()
        .unwrap_or(DEFAULT)
}

/// The production Azure environment called `name` (`PUBLIC`, …).
pub(crate) fn azure_by_name(name: &str) -> Option<Environment> {
    let name = name.to_ascii_uppercase();
    ALL.iter()
        .filter(|e| !e.dns_zone.starts_with(".dev") && !e.dns_zone.starts_with(".staging"))
        .find(|e| e.azure.is_some_and(|a| a.name == name))
        .copied()
}

impl Environment {
    /// Resource for the Azure service-management token (empty off Azure).
    #[must_use]
    pub fn azure_service_management_endpoint(&self) -> &'static str {
        self.azure.map_or("", |a| a.service_management_endpoint)
    }

    /// Azure Resource Manager endpoint (empty off Azure).
    #[must_use]
    pub fn azure_resource_manager_endpoint(&self) -> &'static str {
        self.azure.map_or("", |a| a.resource_manager_endpoint)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hostnames_and_names() {
        assert_eq!(
            for_hostname("adb-1.2.azuredatabricks.net").cloud,
            Cloud::Azure
        );
        assert_eq!(
            for_hostname("adb-1.2.dev.azuredatabricks.net").azure_application_id,
            "62a912ac-b58e-4c1d-89ea-b2dbfc7358fc"
        );
        assert_eq!(for_hostname("x.gcp.databricks.com").cloud, Cloud::Gcp);
        assert_eq!(for_hostname("example.com"), DEFAULT);
        let cn = azure_by_name("china").unwrap();
        assert_eq!(cn.dns_zone, ".databricks.azure.cn");
        assert_eq!(
            cn.azure_resource_manager_endpoint(),
            "https://management.chinacloudapi.cn/"
        );
        assert!(azure_by_name("MARS").is_none());
        assert_eq!(DEFAULT.azure_service_management_endpoint(), "");
        assert_eq!(DEFAULT.azure_resource_manager_endpoint(), "");
        assert_eq!(Cloud::parse("gcp"), Some(Cloud::Gcp));
        assert_eq!(Cloud::parse("aws"), Some(Cloud::Aws));
        assert_eq!(Cloud::parse("?"), None);
    }
}
