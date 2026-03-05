pub mod query;
pub mod subscription;
pub mod types;

use async_graphql::{EmptyMutation, Schema};

use self::query::Query;
use self::subscription::SubscriptionRoot;
use self::types::CoreEvent;

pub type AppSchema = Schema<Query, EmptyMutation, SubscriptionRoot>;

pub fn build_schema(base_path: std::path::PathBuf) -> AppSchema {
    Schema::build(Query, EmptyMutation, SubscriptionRoot)
        .register_output_type::<CoreEvent>()
        .data(base_path)
        .finish()
}
