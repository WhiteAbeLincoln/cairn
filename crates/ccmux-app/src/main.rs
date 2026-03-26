mod api;
mod cli;

use clap::Parser;
use cli::{Cli, Commands};

fn index_db_path() -> std::path::PathBuf {
    dirs::home_dir()
        .expect("Could not determine home directory")
        .join(".claude/ccmux/index.db")
}

fn claude_projects_path() -> std::path::PathBuf {
    dirs::home_dir()
        .expect("Could not determine home directory")
        .join(".claude/projects")
}

fn run_index() -> Result<ccmux_index::IndexStats, Box<dyn std::error::Error>> {
    let db_path = index_db_path();
    if let Some(parent) = db_path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let index = ccmux_index::SearchIndex::open(&db_path)?;
    index.index_all(&claude_projects_path())
}

#[tokio::main]
async fn main() {
    let cli = Cli::parse();

    match cli.command {
        None | Some(Commands::Serve) => {
            tracing_subscriber::fmt()
                .with_env_filter(
                    tracing_subscriber::EnvFilter::try_from_default_env()
                        .unwrap_or_else(|_| "info".into()),
                )
                .init();

            // Start background indexer
            tokio::task::spawn_blocking(|| {
                if let Err(e) = run_index() {
                    tracing::warn!(error = %e, "Background indexing failed");
                }
            });

            let app = api::router();
            let listener = tokio::net::TcpListener::bind("0.0.0.0:3000")
                .await
                .expect("failed to bind to port 3000");

            tracing::info!("listening on {}", listener.local_addr().unwrap());
            axum::serve(listener, app).await.expect("server error");
        }
        Some(Commands::Index) => {
            tracing_subscriber::fmt()
                .with_env_filter(
                    tracing_subscriber::EnvFilter::from_default_env()
                        .add_directive("ccmux=info".parse().unwrap()),
                )
                .init();

            match run_index() {
                Ok(stats) => {
                    println!(
                        "Indexed {} sessions ({} messages, {} files) in {:.1}s",
                        stats.sessions_indexed,
                        stats.messages_indexed,
                        stats.files_indexed,
                        stats.duration.as_secs_f64()
                    );
                }
                Err(e) => {
                    eprintln!("Indexing failed: {e}");
                    std::process::exit(1);
                }
            }
        }
        Some(Commands::Search {
            query,
            limit,
            project,
            after,
            before,
            files,
            json,
        }) => {
            run_search(query, limit, project, after, before, files, json);
        }
    }
}

fn run_search(
    query: String,
    limit: usize,
    project: Option<String>,
    after: Option<String>,
    before: Option<String>,
    files: bool,
    json: bool,
) {
    let index = match ccmux_index::SearchIndex::open(&index_db_path()) {
        Ok(idx) => idx,
        Err(e) => {
            eprintln!("Failed to open index: {e}");
            eprintln!("Run 'ccmux index' first to build the search index.");
            std::process::exit(1);
        }
    };

    if files {
        match index.search_files(&query) {
            Ok(results) => {
                if json {
                    println!("{}", serde_json::to_string_pretty(&results).unwrap());
                } else {
                    if results.is_empty() {
                        println!("No files matching \"{query}\"");
                        return;
                    }
                    println!("# Files matching \"{query}\"\n");
                    for result in &results {
                        println!("- {} (session: {})", result.file_path, result.session_id);
                    }
                }
            }
            Err(e) => {
                eprintln!("Search failed: {e}");
                std::process::exit(1);
            }
        }
        return;
    }

    let search_query = ccmux_index::SearchQuery {
        text: query.clone(),
        project,
        after,
        before,
        limit,
    };

    match index.search(&search_query) {
        Ok(results) => {
            if json {
                println!("{}", serde_json::to_string_pretty(&results).unwrap());
            } else {
                use ccmux_core::display::markdown::{SearchResultGroup, render_search_results};
                use ccmux_core::display::{DisplayItem, DisplayItemWithMode, ItemMeta};

                let total_matches: usize = results.iter().map(|r| r.matches.len()).sum();
                let total_sessions = results.len();

                let groups: Vec<SearchResultGroup> = results
                    .iter()
                    .map(|r| {
                        let items: Vec<DisplayItemWithMode> = r
                            .matches
                            .iter()
                            .map(|m| {
                                let item = match m.role.as_str() {
                                    "user" => DisplayItem::UserMessage {
                                        content: m.content.clone(),
                                        meta: ItemMeta {
                                            uuid: Some(m.event_uuid.clone()),
                                            model: None,
                                            tokens: None,
                                        },
                                        raw: serde_json::Value::Null,
                                        cursor: None,
                                    },
                                    _ => DisplayItem::AssistantMessage {
                                        text: m.content.clone(),
                                        meta: ItemMeta {
                                            uuid: Some(m.event_uuid.clone()),
                                            model: None,
                                            tokens: None,
                                        },
                                        raw: serde_json::Value::Null,
                                        cursor: None,
                                    },
                                };
                                DisplayItemWithMode::Full(item)
                            })
                            .collect();

                        SearchResultGroup {
                            session_id: r.session_id.clone(),
                            slug: r.slug.clone(),
                            project_path: r.project_path.clone(),
                            created_at: r.created_at.clone(),
                            items,
                        }
                    })
                    .collect();

                let output = render_search_results(&query, &groups, total_matches, total_sessions);
                print!("{output}");
            }
        }
        Err(e) => {
            eprintln!("Search failed: {e}");
            std::process::exit(1);
        }
    }
}
