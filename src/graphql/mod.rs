pub mod query;
pub mod types;

use async_graphql::{EmptyMutation, EmptySubscription, Schema};

use self::query::Query;
use self::types::CoreEvent;

pub type AppSchema = Schema<Query, EmptyMutation, EmptySubscription>;

pub fn build_schema(base_path: std::path::PathBuf) -> AppSchema {
    Schema::build(Query, EmptyMutation, EmptySubscription)
        .register_output_type::<CoreEvent>()
        .data(base_path)
        .finish()
}
