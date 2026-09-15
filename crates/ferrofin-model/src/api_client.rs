//! Port of `MediaBrowser.Model.ApiClient`.

use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

/// The server discovery info model.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "PascalCase")]
pub struct ServerDiscoveryInfo {
    /// Gets the address.
    pub address: String,

    /// Gets the server identifier.
    pub id: String,

    /// Gets the name.
    pub name: String,

    /// Gets the endpoint address (explicitly null in UDP discovery replies).
    pub endpoint_address: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn discovery_serializes_all_four_keys_including_null_endpoint() {
        let response = ServerDiscoveryInfo {
            address: "http://192.168.1.2:8096".into(),
            id: "persisted-server-id".into(),
            name: "Living room".into(),
            endpoint_address: None,
        };
        let json = serde_json::to_value(&response).expect("serialize discovery");
        assert_eq!(
            json,
            serde_json::json!({
                "Address": "http://192.168.1.2:8096",
                "Id": "persisted-server-id",
                "Name": "Living room",
                "EndpointAddress": null,
            })
        );
        assert_eq!(
            serde_json::from_value::<ServerDiscoveryInfo>(json).expect("deserialize discovery"),
            response
        );
    }
}
