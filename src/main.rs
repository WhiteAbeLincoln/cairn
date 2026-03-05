use std::path::PathBuf;

use async_graphql::http::GraphiQLSource;
use async_graphql_axum::{GraphQLRequest, GraphQLResponse, GraphQLSubscription};
use axum::{
    Extension,
    response::{Html, IntoResponse},
    routing::get,
};
use tower_http::cors::CorsLayer;
use tower_http::trace::TraceLayer;
use tracing_subscriber::{EnvFilter, fmt};

use ccmux::graphql::AppSchema;

async fn graphql_handler(schema: Extension<AppSchema>, req: GraphQLRequest) -> GraphQLResponse {
    schema.execute(req.into_inner()).await.into()
}

async fn graphiql() -> impl IntoResponse {
    Html(
        GraphiQLSource::build()
            .endpoint("/graphql")
            .subscription_endpoint("ws://localhost:3001/graphql/ws")
            .finish(),
    )
}

#[tokio::main]
async fn main() {
    fmt()
        .with_env_filter(EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()))
        .init();

    let home = std::env::var("HOME").expect("HOME not set");
    let base_path = PathBuf::from(format!("{home}/.claude/projects"));

    let schema = ccmux::graphql::build_schema(base_path);

    let app = axum::Router::new()
        .route("/graphql", get(graphiql).post(graphql_handler))
        .route_service("/graphql/ws", GraphQLSubscription::new(schema.clone()))
        .layer(Extension(schema))
        .layer(TraceLayer::new_for_http())
        .layer(CorsLayer::permissive());

    let addr = "127.0.0.1:3001";
    tracing::info!("Server running at http://{addr}");
    tracing::info!("GraphiQL playground at http://{addr}/graphql");

    let listener = tokio::net::TcpListener::bind(addr).await.unwrap();
    axum::serve(listener, app).await.unwrap();
}
