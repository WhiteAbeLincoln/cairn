use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(name = "ccmux", version, about = "Session log viewer for Claude Code")]
pub struct Cli {
    #[command(subcommand)]
    pub command: Option<Commands>,
}

#[derive(Subcommand)]
pub enum Commands {
    /// Start the web server with background indexing
    Serve,
    /// Build or update the search index and exit
    Index,
    /// Search indexed sessions
    Search {
        /// Search query
        query: String,
        /// Maximum number of results
        #[arg(long, default_value = "20")]
        limit: usize,
        /// Filter to a specific project path
        #[arg(long)]
        project: Option<String>,
        /// Only sessions created after this date (ISO 8601)
        #[arg(long)]
        after: Option<String>,
        /// Only sessions created before this date (ISO 8601)
        #[arg(long)]
        before: Option<String>,
        /// Search file paths instead of message content
        #[arg(long)]
        files: bool,
        /// Output JSON instead of markdown
        #[arg(long)]
        json: bool,
    },
}
