//! Auto-generate axum routes from ResourceDef.
//!
//! Each ResourceDef produces REST routes:
//! - CRUD: GET /list, POST /create, GET /:id, DELETE /:id
//! - Actions: POST /:id/:action or POST /:action (for resource-level actions)
//!
//! The generated routes all call into the same `HandlerFn` from the registry.

use axum::extract::{Json, Path, Query};
use axum::response::IntoResponse;
use axum::routing::{delete, get, post};
use axum::Router;
use std::collections::HashMap;
use std::sync::Arc;

use super::error_response::ApiError;
use crate::error::SyfrahError;
use crate::resource::{
    OperationRequest, OperationResponse, OperationSemantics, ResourceRegistration, ScopeValues,
};

/// State shared across all route handlers.
pub struct ApiState {
    pub registrations: Vec<Arc<ResourceRegistration>>,
}

/// Build an axum Router from a set of resource registrations.
///
/// For each resource:
/// - CRUD operations → standard REST routes
/// - Custom actions → POST routes
///
/// All routes are prefixed with `/admin/v1` or `/v1`.
pub fn build_router(registrations: Vec<ResourceRegistration>, prefix: &str) -> Router {
    let mut router = Router::new();

    let shared: Vec<Arc<ResourceRegistration>> = registrations.into_iter().map(Arc::new).collect();

    for reg in &shared {
        let kind = reg.def.identity.cli_name;
        let base = format!("{prefix}/{kind}");

        for op in &reg.def.operations {
            let reg_clone = Arc::clone(reg);
            let op_name = op.name.to_string();

            match &op.semantics {
                OperationSemantics::List => {
                    let r = reg_clone;
                    let name = op_name.clone();
                    router = router.route(
                        &base,
                        get(move |Query(params): Query<HashMap<String, String>>| {
                            let r = Arc::clone(&r);
                            let name = name.clone();
                            async move { handle_operation(&r, &name, None, params).await }
                        }),
                    );
                }
                OperationSemantics::Create => {
                    let r = reg_clone;
                    let name = op_name.clone();
                    router = router.route(
                        &base,
                        post(move |Json(body): Json<HashMap<String, String>>| {
                            let r = Arc::clone(&r);
                            let name = name.clone();
                            async move {
                                let resource_name = body.get("name").cloned();
                                handle_operation(&r, &name, resource_name, body).await
                            }
                        }),
                    );
                }
                OperationSemantics::Get => {
                    let r = reg_clone;
                    let name = op_name.clone();
                    let route = format!("{base}/{{id}}");
                    router =
                        router.route(
                            &route,
                            get(move |Path(id): Path<String>| {
                                let r = Arc::clone(&r);
                                let name = name.clone();
                                async move {
                                    handle_operation(&r, &name, Some(id), HashMap::new()).await
                                }
                            }),
                        );
                }
                OperationSemantics::Delete => {
                    let r = reg_clone;
                    let name = op_name.clone();
                    let route = format!("{base}/{{id}}");
                    router =
                        router.route(
                            &route,
                            delete(move |Path(id): Path<String>| {
                                let r = Arc::clone(&r);
                                let name = name.clone();
                                async move {
                                    handle_operation(&r, &name, Some(id), HashMap::new()).await
                                }
                            }),
                        );
                }
                OperationSemantics::Action => {
                    let r = reg_clone;
                    let name = op_name.clone();
                    let route = format!("{base}/{}", op.name);
                    router = router.route(
                        &route,
                        post(move |Json(body): Json<HashMap<String, String>>| {
                            let r = Arc::clone(&r);
                            let name = name.clone();
                            async move {
                                let resource_name = body.get("name").cloned();
                                handle_operation(&r, &name, resource_name, body).await
                            }
                        }),
                    );
                }
                OperationSemantics::Update { .. } => {
                    let r = reg_clone;
                    let name = op_name.clone();
                    let route = format!("{base}/{{id}}");
                    router = router.route(
                        &route,
                        axum::routing::patch(move |Path(id): Path<String>, Json(body): Json<HashMap<String, String>>| {
                            let r = Arc::clone(&r);
                            let name = name.clone();
                            async move {
                                handle_operation(&r, &name, Some(id), body).await
                            }
                        }),
                    );
                }
            }
        }
    }

    router
}

/// Execute an operation via the registered handler and return HTTP response.
async fn handle_operation(
    reg: &ResourceRegistration,
    operation: &str,
    name: Option<String>,
    fields: HashMap<String, String>,
) -> impl IntoResponse {
    // Validate constraints
    let op_def = reg.def.operations.iter().find(|o| o.name == operation);
    if let Some(op_def) = op_def {
        for constraint in &op_def.constraints {
            if let Err(msg) = constraint.validate(&fields) {
                return Err(ApiError(SyfrahError::validation(msg)));
            }
        }
    }

    let request = OperationRequest {
        operation: operation.to_string(),
        name,
        scope: ScopeValues::default(),
        fields,
    };

    let response = (reg.handler)(request)
        .await
        .map_err(|e: anyhow::Error| ApiError(SyfrahError::internal(e.to_string())))?;

    match response {
        OperationResponse::Resource(v) => Ok(axum::Json(v).into_response()),
        OperationResponse::ResourceList(items) => Ok(axum::Json(serde_json::json!({
            "items": items,
            "count": items.len(),
        }))
        .into_response()),
        OperationResponse::Message(msg) => {
            Ok(axum::Json(serde_json::json!({"message": msg})).into_response())
        }
        OperationResponse::None => Ok(axum::http::StatusCode::NO_CONTENT.into_response()),
    }
}

/// Generate a list of all routes for documentation.
pub fn list_routes(registrations: &[ResourceRegistration], prefix: &str) -> Vec<RouteInfo> {
    let mut routes = Vec::new();

    for reg in registrations {
        let kind = reg.def.identity.cli_name;
        let base = format!("{prefix}/{kind}");

        for op in &reg.def.operations {
            let (method, path) = match &op.semantics {
                OperationSemantics::List => ("GET", base.clone()),
                OperationSemantics::Create => ("POST", base.clone()),
                OperationSemantics::Get => ("GET", format!("{base}/{{id}}")),
                OperationSemantics::Delete => ("DELETE", format!("{base}/{{id}}")),
                OperationSemantics::Update { .. } => ("PATCH", format!("{base}/{{id}}")),
                OperationSemantics::Action => ("POST", format!("{base}/{}", op.name)),
            };

            routes.push(RouteInfo {
                method: method.to_string(),
                path,
                operation: op.name.to_string(),
                resource: kind.to_string(),
                description: op.description.to_string(),
            });
        }
    }

    routes
}

/// Information about a generated route.
#[derive(Debug, Clone, serde::Serialize)]
pub struct RouteInfo {
    pub method: String,
    pub path: String,
    pub operation: String,
    pub resource: String,
    pub description: String,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::resource::*;

    fn test_resource() -> ResourceRegistration {
        let def = ResourceDef {
            identity: ResourceIdentity {
                kind: "widget",
                cli_name: "widget",
                plural: "widgets",
                description: "Test widget",
                aliases: &[],
            },
            scope: ScopeDef::global(),
            schema: ResourceSchema::new(),
            operations: vec![
                OperationDef::create(),
                OperationDef::list(),
                OperationDef::get(),
                OperationDef::delete(),
                OperationDef::action("polish", "Polish the widget"),
            ],
            presentation: PresentationDef::none(),
        };

        let handler: HandlerFn = Box::new(|req| {
            Box::pin(async move {
                match req.operation.as_str() {
                    "list" => Ok(OperationResponse::ResourceList(vec![
                        serde_json::json!({"name": "w1"}),
                    ])),
                    "create" => Ok(OperationResponse::Resource(
                        serde_json::json!({"name": req.name.unwrap_or_default()}),
                    )),
                    "get" => Ok(OperationResponse::Resource(
                        serde_json::json!({"name": req.name.unwrap_or_default()}),
                    )),
                    "delete" => Ok(OperationResponse::Message("deleted".into())),
                    "polish" => Ok(OperationResponse::Message("polished".into())),
                    _ => Ok(OperationResponse::None),
                }
            })
        });

        ResourceRegistration { def, handler }
    }

    #[test]
    fn list_routes_generates_all() {
        let reg = test_resource();
        let routes = list_routes(&[reg], "/admin/v1");

        assert_eq!(routes.len(), 5);

        let methods: Vec<&str> = routes.iter().map(|r| r.method.as_str()).collect();
        assert!(methods.contains(&"GET"));
        assert!(methods.contains(&"POST"));
        assert!(methods.contains(&"DELETE"));

        let paths: Vec<&str> = routes.iter().map(|r| r.path.as_str()).collect();
        assert!(paths.contains(&"/admin/v1/widget"));
        assert!(paths.contains(&"/admin/v1/widget/{id}"));
        assert!(paths.contains(&"/admin/v1/widget/polish"));
    }

    #[test]
    fn list_routes_empty() {
        let routes = list_routes(&[], "/v1");
        assert!(routes.is_empty());
    }

    #[test]
    fn build_router_does_not_panic() {
        let reg = test_resource();
        let _router = build_router(vec![reg], "/admin/v1");
    }

    #[tokio::test]
    async fn handle_operation_list() {
        let reg = test_resource();
        let resp = handle_operation(&reg, "list", None, HashMap::new()).await;
        assert!(resp.into_response().status().is_success());
    }

    #[tokio::test]
    async fn handle_operation_create() {
        let reg = test_resource();
        let mut fields = HashMap::new();
        fields.insert("color".into(), "red".into());
        let resp = handle_operation(&reg, "create", Some("w1".into()), fields).await;
        assert!(resp.into_response().status().is_success());
    }

    #[tokio::test]
    async fn handle_operation_delete() {
        let reg = test_resource();
        let resp = handle_operation(&reg, "delete", Some("w1".into()), HashMap::new()).await;
        assert!(resp.into_response().status().is_success());
    }

    #[tokio::test]
    async fn handle_operation_action() {
        let reg = test_resource();
        let resp = handle_operation(&reg, "polish", Some("w1".into()), HashMap::new()).await;
        assert!(resp.into_response().status().is_success());
    }

    #[test]
    fn route_info_serializes() {
        let ri = RouteInfo {
            method: "GET".into(),
            path: "/admin/v1/widget".into(),
            operation: "list".into(),
            resource: "widget".into(),
            description: "List widgets".into(),
        };
        let json = serde_json::to_string(&ri).unwrap();
        assert!(json.contains("GET"));
    }
}
