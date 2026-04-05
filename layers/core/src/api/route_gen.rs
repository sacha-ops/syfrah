//! Auto-generate axum routes from ResourceDef.
//!
//! Fixes:
//! - #11: GET and DELETE on same /{id} path use method routing (not separate routes)
//! - #10: Scoped routes supported via parent refs
//! - #3: Pagination via query params on list endpoints

use axum::extract::{Json, Path, Query};
use axum::response::IntoResponse;
use axum::routing::{get, post, MethodRouter};
use axum::Router;
use std::collections::HashMap;
use std::sync::Arc;

use super::error_response::ApiError;
use crate::error::SyfrahError;
use crate::resource::{
    OperationRequest, OperationResponse, OperationSemantics, ResourceRegistration, ScopeValues,
};

/// Build an axum Router from resource registrations.
///
/// For resources with scope parents, generates nested routes:
/// `/{prefix}/{parent}/{parent_id}/{resource}` etc.
pub fn build_router(registrations: Vec<ResourceRegistration>, prefix: &str) -> Router {
    let mut router = Router::new();

    let shared: Vec<Arc<ResourceRegistration>> = registrations.into_iter().map(Arc::new).collect();

    for reg in &shared {
        let kind = reg.def.identity.cli_name;

        // #10: Build route path from scope parents
        let base = build_base_path(prefix, kind, &reg.def.scope.parents);

        // Collect which methods we need on /{base} and /{base}/{id}
        let mut collection_methods: Option<MethodRouter> = None;
        let mut instance_methods: Option<MethodRouter> = None;

        for op in &reg.def.operations {
            let r = Arc::clone(reg);
            let op_name = op.name.to_string();

            match &op.semantics {
                // #3: List with pagination query params
                OperationSemantics::List => {
                    let name = op_name.clone();
                    let handler = get(move |Query(params): Query<HashMap<String, String>>| {
                        let r = Arc::clone(&r);
                        let name = name.clone();
                        async move { handle_operation(&r, &name, None, params).await }
                    });
                    collection_methods = Some(match collection_methods {
                        Some(m) => m.merge(handler),
                        None => handler,
                    });
                }
                OperationSemantics::Create => {
                    let name = op_name.clone();
                    let handler = post(move |Json(body): Json<HashMap<String, String>>| {
                        let r = Arc::clone(&r);
                        let name = name.clone();
                        async move {
                            let resource_name = body.get("name").cloned();
                            handle_operation(&r, &name, resource_name, body).await
                        }
                    });
                    collection_methods = Some(match collection_methods {
                        Some(m) => m.merge(handler),
                        None => handler,
                    });
                }
                // #11: GET and DELETE on same path — merged into one MethodRouter
                OperationSemantics::Get => {
                    let name = op_name.clone();
                    let handler = get(move |Path(id): Path<String>| {
                        let r = Arc::clone(&r);
                        let name = name.clone();
                        async move { handle_operation(&r, &name, Some(id), HashMap::new()).await }
                    });
                    instance_methods = Some(match instance_methods {
                        Some(m) => m.merge(handler),
                        None => handler,
                    });
                }
                OperationSemantics::Delete => {
                    let name = op_name.clone();
                    let handler = axum::routing::delete(move |Path(id): Path<String>| {
                        let r = Arc::clone(&r);
                        let name = name.clone();
                        async move { handle_operation(&r, &name, Some(id), HashMap::new()).await }
                    });
                    instance_methods = Some(match instance_methods {
                        Some(m) => m.merge(handler),
                        None => handler,
                    });
                }
                OperationSemantics::Update { .. } => {
                    let name = op_name.clone();
                    let handler = axum::routing::patch(
                        move |Path(id): Path<String>, Json(body): Json<HashMap<String, String>>| {
                            let r = Arc::clone(&r);
                            let name = name.clone();
                            async move { handle_operation(&r, &name, Some(id), body).await }
                        },
                    );
                    instance_methods = Some(match instance_methods {
                        Some(m) => m.merge(handler),
                        None => handler,
                    });
                }
                OperationSemantics::Action => {
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
            }
        }

        // Register merged routes
        if let Some(methods) = collection_methods {
            router = router.route(&base, methods);
        }
        if let Some(methods) = instance_methods {
            let instance_path = format!("{base}/{{id}}");
            router = router.route(&instance_path, methods);
        }
    }

    router
}

/// #10: Build a route path incorporating scope parents.
///
/// No parents: `/admin/v1/org`
/// With parents: `/admin/v1/org/{org_id}/project/{project_id}/vpc`
fn build_base_path(prefix: &str, kind: &str, parents: &[crate::resource::ParentRef]) -> String {
    if parents.is_empty() {
        return format!("{prefix}/{kind}");
    }

    let mut path = prefix.to_string();
    for parent in parents {
        let parent_kind = parent.kind;
        let param = format!("{{{parent_kind}_id}}");
        path = format!("{path}/{parent_kind}/{param}");
    }
    format!("{path}/{kind}")
}

async fn handle_operation(
    reg: &ResourceRegistration,
    operation: &str,
    name: Option<String>,
    fields: HashMap<String, String>,
) -> impl IntoResponse {
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
        let base = build_base_path(prefix, kind, &reg.def.scope.parents);

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

/// #7: OpenAPI-compatible route listing.
#[derive(Debug, Clone, serde::Serialize)]
pub struct RouteInfo {
    pub method: String,
    pub path: String,
    pub operation: String,
    pub resource: String,
    pub description: String,
}

/// #7: Generate a minimal OpenAPI-style spec from routes.
pub fn openapi_spec(registrations: &[ResourceRegistration], prefix: &str) -> serde_json::Value {
    let routes = list_routes(registrations, prefix);

    let mut paths = serde_json::Map::new();
    for route in &routes {
        let path_entry = paths
            .entry(route.path.clone())
            .or_insert_with(|| serde_json::json!({}));
        let method_lower = route.method.to_lowercase();
        path_entry[method_lower] = serde_json::json!({
            "summary": route.description,
            "operationId": format!("{}_{}", route.resource, route.operation),
            "tags": [route.resource],
        });
    }

    serde_json::json!({
        "openapi": "3.0.0",
        "info": {
            "title": "Syfrah API",
            "version": env!("CARGO_PKG_VERSION"),
        },
        "paths": paths,
    })
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

    fn scoped_resource() -> ResourceRegistration {
        let def = ResourceDef {
            identity: ResourceIdentity {
                kind: "subnet",
                cli_name: "subnet",
                plural: "subnets",
                description: "Subnet",
                aliases: &[],
            },
            scope: ScopeDef::within("vpc", "--vpc", "Parent VPC"),
            schema: ResourceSchema::new(),
            operations: vec![OperationDef::list(), OperationDef::create()],
            presentation: PresentationDef::none(),
        };

        let handler: HandlerFn =
            Box::new(|_req| Box::pin(async move { Ok(OperationResponse::ResourceList(vec![])) }));

        ResourceRegistration { def, handler }
    }

    // ── Route generation ──

    #[test]
    fn list_routes_generates_all() {
        let routes = list_routes(&[test_resource()], "/admin/v1");
        assert_eq!(routes.len(), 5);
        let methods: Vec<&str> = routes.iter().map(|r| r.method.as_str()).collect();
        assert!(methods.contains(&"GET"));
        assert!(methods.contains(&"POST"));
        assert!(methods.contains(&"DELETE"));
    }

    #[test]
    fn list_routes_empty() {
        assert!(list_routes(&[], "/v1").is_empty());
    }

    // #11: GET + DELETE on same path
    #[test]
    fn build_router_with_get_and_delete() {
        let reg = test_resource();
        let _router = build_router(vec![reg], "/admin/v1"); // should not panic
    }

    // #10: Scoped routes
    #[test]
    fn scoped_routes_include_parent() {
        let routes = list_routes(&[scoped_resource()], "/admin/v1");
        let paths: Vec<&str> = routes.iter().map(|r| r.path.as_str()).collect();
        assert!(
            paths
                .iter()
                .any(|p| p.contains("vpc") && p.contains("subnet")),
            "expected scoped path, got: {paths:?}"
        );
    }

    #[test]
    fn base_path_no_parents() {
        let path = build_base_path("/admin/v1", "org", &[]);
        assert_eq!(path, "/admin/v1/org");
    }

    #[test]
    fn base_path_with_parent() {
        let parents = vec![crate::resource::ParentRef {
            kind: "org",
            flag: "--org",
            required_on_create: true,
            required_on_resolve: false,
            description: "Organization",
        }];
        let path = build_base_path("/admin/v1", "project", &parents);
        assert_eq!(path, "/admin/v1/org/{org_id}/project");
    }

    #[test]
    fn base_path_nested() {
        let parents = vec![
            crate::resource::ParentRef {
                kind: "org",
                flag: "--org",
                required_on_create: true,
                required_on_resolve: false,
                description: "Org",
            },
            crate::resource::ParentRef {
                kind: "project",
                flag: "--project",
                required_on_create: true,
                required_on_resolve: false,
                description: "Project",
            },
        ];
        let path = build_base_path("/v1", "vpc", &parents);
        assert_eq!(path, "/v1/org/{org_id}/project/{project_id}/vpc");
    }

    // ── Handler ──

    #[tokio::test]
    async fn handle_operation_list() {
        let reg = test_resource();
        let resp = handle_operation(&reg, "list", None, HashMap::new()).await;
        assert!(resp.into_response().status().is_success());
    }

    #[tokio::test]
    async fn handle_operation_create() {
        let reg = test_resource();
        let resp = handle_operation(&reg, "create", Some("w1".into()), HashMap::new()).await;
        assert!(resp.into_response().status().is_success());
    }

    #[tokio::test]
    async fn handle_operation_delete() {
        let reg = test_resource();
        let resp = handle_operation(&reg, "delete", Some("w1".into()), HashMap::new()).await;
        assert!(resp.into_response().status().is_success());
    }

    // #7: OpenAPI spec
    #[test]
    fn openapi_spec_generates() {
        let spec = openapi_spec(&[test_resource()], "/admin/v1");
        assert_eq!(spec["openapi"], "3.0.0");
        assert!(spec["paths"]["/admin/v1/widget"]["get"].is_object());
        assert!(spec["paths"]["/admin/v1/widget"]["post"].is_object());
        assert!(spec["paths"]["/admin/v1/widget/{id}"]["get"].is_object());
        assert!(spec["paths"]["/admin/v1/widget/{id}"]["delete"].is_object());
    }

    #[test]
    fn openapi_spec_empty() {
        let spec = openapi_spec(&[], "/v1");
        assert_eq!(spec["openapi"], "3.0.0");
        assert!(spec["paths"].as_object().unwrap().is_empty());
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
