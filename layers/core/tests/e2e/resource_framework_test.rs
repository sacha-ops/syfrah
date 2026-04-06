//! E2E test: resource framework generates CLI + API from a single definition.

use syfrah_core::resource::*;

fn test_resource() -> ResourceRegistration {
    let def = ResourceDef::build("widget", "Test widget")
        .alias("w")
        .plural("widgets")
        .action("create", "Create a widget")
            .op(|op| op
                .with_arg(OperationArg::required("name", FieldDef::string("name", "Widget name")))
            )
        .action("list", "List widgets")
        .list()
        .get()
        .column("NAME", "name")
        .column("STATE", "state")
        .done();

    let handler: HandlerFn = Box::new(|req| {
        Box::pin(async move {
            match req.operation.as_str() {
                "list" => Ok(OperationResponse::ResourceList(vec![
                    serde_json::json!({"name": "w1", "state": "active"}),
                ])),
                "get" => Ok(OperationResponse::Resource(
                    serde_json::json!({"name": req.name.unwrap_or_default()}),
                )),
                "create" => Ok(OperationResponse::Resource(
                    serde_json::json!({"name": req.fields.get("name").cloned().unwrap_or_default()}),
                )),
                _ => Ok(OperationResponse::None),
            }
        })
    });

    ResourceRegistration { def, handler }
}

#[test]
fn cli_generation() {
    let reg = test_resource();
    let cmd = generate_command(&reg.def);

    assert_eq!(cmd.get_name(), "widget");

    // Should have subcommands for all operations
    let subs: Vec<&str> = cmd.get_subcommands().map(|c| c.get_name()).collect();
    assert!(subs.contains(&"create"));
    assert!(subs.contains(&"list"));
    assert!(subs.contains(&"get"));
}

#[test]
fn api_route_generation() {
    let reg = test_resource();
    let routes = syfrah_core::api::list_routes(&[reg], "/v1");

    let ops: Vec<&str> = routes.iter().map(|r| r.operation.as_str()).collect();
    assert!(ops.contains(&"create"));
    assert!(ops.contains(&"list"));
    assert!(ops.contains(&"get"));

    // List is GET /v1/widget
    assert!(routes.iter().any(|r| r.method == "GET" && r.path == "/v1/widget"));
}

#[test]
fn openapi_spec_generation() {
    let reg = test_resource();
    let spec = syfrah_core::api::openapi_spec(&[reg], "/v1");

    assert_eq!(spec["openapi"], "3.0.0");
    assert!(spec["paths"]["/v1/widget"].is_object());
}

#[tokio::test]
async fn api_server_serves_routes() {
    use axum::body::Body;
    use http::Request;
    use syfrah_core::api::{ApiConfig, ApiServer};
    use tower::ServiceExt;

    let server = ApiServer::new(ApiConfig::default(), vec![test_resource()], vec![]);

    // GET /admin/v1/widget → list
    let req = Request::builder()
        .uri("/admin/v1/widget")
        .body(Body::empty())
        .unwrap();
    let resp = server.admin_router().clone().oneshot(req).await.unwrap();
    assert_eq!(resp.status(), 200);

    // GET /health
    let req = Request::builder()
        .uri("/health")
        .body(Body::empty())
        .unwrap();
    let resp = server.admin_router().clone().oneshot(req).await.unwrap();
    assert_eq!(resp.status(), 200);
}

#[test]
fn registry_find_by_alias() {
    let mut registry = ResourceRegistry::new();
    registry.register(test_resource());

    assert!(registry.find("widget").is_some());
    assert!(registry.find("w").is_some());
    assert!(registry.find("unknown").is_none());
}
