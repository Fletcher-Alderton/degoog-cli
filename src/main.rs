use anyhow::Context as _;
use clap::{Args, Parser, Subcommand};
use degoog_cli::cache::Cache;
use degoog_cli::config::{self, CacheConfig, Overrides};
use degoog_cli::log::{self, Level};
use degoog_cli::output::ToolResult;
use degoog_cli::tools::{bundle, info, scrape, search, Ctx};
use degoog_cli::{mcp, text};
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::Arc;

/// Search a remote Degoog instance from your terminal, or serve the same
/// tools to a local MCP client over stdio.
#[derive(Parser)]
#[command(name = "degoog", version, about, propagate_version = true)]
struct Cli {
    #[command(flatten)]
    global: Global,
    #[command(subcommand)]
    command: Cmd,
}

#[derive(Args)]
struct Global {
    /// Degoog base URL [env: DEGOOG_URL]
    #[arg(long, global = true)]
    url: Option<String>,
    /// Degoog API key. Prefer the DEGOOG_API_KEY environment variable,
    /// command-line arguments are visible to other local users.
    #[arg(long, global = true, hide_env_values = true)]
    api_key: Option<String>,
    /// Config file [env: DEGOOG_CONFIG] [default: ~/.config/degoog/config.toml]
    #[arg(long, global = true)]
    config: Option<PathBuf>,
    /// Print structured JSON instead of text
    #[arg(long, global = true)]
    json: bool,
    /// Do not read or write the on-disk cache
    #[arg(long, global = true)]
    no_cache: bool,
    /// More logging on stderr (-v info, -vv debug)
    #[arg(short, long, global = true, action = clap::ArgAction::Count)]
    verbose: u8,
    /// Only log errors
    #[arg(long, global = true)]
    quiet: bool,
}

#[derive(Subcommand)]
enum Cmd {
    /// Run a Degoog search
    #[command(visible_alias = "s")]
    Search(SearchCmd),
    /// Fetch URLs and print cleaned evidence chunks
    Scrape(ScrapeCmd),
    /// Search, read the top sources, and print one evidence pack
    #[command(visible_alias = "b")]
    Bundle(BundleCmd),
    /// List search types, engines and bang commands on the instance
    Discover,
    /// Re-run one engine for a query
    Retry(RetryCmd),
    /// Run a bang command, e.g. `degoog command '!weather berlin'`
    #[command(visible_alias = "bang")]
    Command(CommandCmd),
    /// Check that the Degoog instance is reachable
    Health,
    /// Serve the tools to a local MCP client over stdio
    Mcp,
    /// Manage the config file
    #[command(subcommand)]
    Config(ConfigCmd),
}

#[derive(Args)]
struct SearchCmd {
    /// Search query
    #[arg(required = true, num_args = 1..)]
    query: Vec<String>,
    /// Search type, e.g. web, images, news (see `degoog discover`)
    #[arg(short = 't', long = "type")]
    kind: Option<String>,
    /// Result page, 1-10
    #[arg(short, long)]
    page: Option<u32>,
    /// Time filter: any, hour, day, week, month, year, custom
    #[arg(long)]
    time: Option<String>,
    /// Language code, e.g. en
    #[arg(short, long)]
    lang: Option<String>,
    /// YYYY-MM-DD, with --time custom
    #[arg(long)]
    from: Option<String>,
    /// YYYY-MM-DD, with --time custom
    #[arg(long)]
    to: Option<String>,
    /// Engine id to use, repeatable
    #[arg(short, long = "engine")]
    engines: Vec<String>,
    /// Safe search level
    #[arg(long)]
    safe: Option<String>,
    /// Cap on returned results, 1-50
    #[arg(short = 'n', long)]
    max_results: Option<usize>,
}

#[derive(Args)]
struct ScrapeCmd {
    /// URLs to fetch
    #[arg(required = true, num_args = 1..)]
    urls: Vec<String>,
    /// Query used to pick the most relevant chunks
    #[arg(short, long)]
    query: Option<String>,
    /// Per-URL character budget, 200-20000
    #[arg(long)]
    max_chars: Option<usize>,
    /// Per-URL chunk cap, 1-20
    #[arg(long)]
    max_chunks: Option<usize>,
}

#[derive(Args)]
struct BundleCmd {
    /// Research query
    #[arg(num_args = 0..)]
    query: Vec<String>,
    /// Additional query, repeatable
    #[arg(short = 'Q', long = "also")]
    also: Vec<String>,
    /// Search type
    #[arg(short = 't', long = "type")]
    kind: Option<String>,
    /// Language code
    #[arg(short, long)]
    lang: Option<String>,
    /// Time filter
    #[arg(long)]
    time: Option<String>,
    /// Add queries from Degoog suggestions
    #[arg(long, conflicts_with = "no_expand")]
    expand: bool,
    /// Never expand queries, even if the config says so
    #[arg(long)]
    no_expand: bool,
    /// How many sources to read, 1-12
    #[arg(short = 'n', long)]
    sources: Option<usize>,
    /// Total evidence character budget, 500-40000
    #[arg(long)]
    max_chars: Option<usize>,
}

#[derive(Args)]
struct RetryCmd {
    /// Engine id or name
    engine: String,
    /// Query
    #[arg(required = true, num_args = 1..)]
    query: Vec<String>,
    #[arg(short = 't', long = "type")]
    kind: Option<String>,
    #[arg(short, long)]
    page: Option<u32>,
    #[arg(short, long)]
    lang: Option<String>,
}

#[derive(Args)]
struct CommandCmd {
    /// Bang command and arguments
    #[arg(required = true, num_args = 1..)]
    query: Vec<String>,
    #[arg(short, long)]
    page: Option<u32>,
}

#[derive(Subcommand)]
enum ConfigCmd {
    /// Write a commented config file
    Init {
        /// Overwrite an existing file
        #[arg(long)]
        force: bool,
    },
    /// Print the config file path
    Path,
    /// Print the effective config (file + environment + flags)
    Show,
}

fn set_log_level(global: &Global) {
    let level = if global.quiet {
        Level::Error
    } else {
        match global.verbose {
            0 => std::env::var(config::env::LOG_LEVEL).ok().and_then(|v| Level::parse(&v)).unwrap_or(Level::Warn),
            1 => Level::Info,
            _ => Level::Debug,
        }
    };
    log::set_level(level);
}

fn print_result(result: &ToolResult, json: bool) -> ExitCode {
    if json {
        println!("{}", serde_json::to_string_pretty(&result.structured).unwrap_or_default());
    } else if result.is_error {
        eprintln!("error: {}", result.text);
    } else {
        println!("{}", result.text);
    }
    if result.is_error {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    }
}

fn config_command(cmd: ConfigCmd, overrides: &Overrides) -> anyhow::Result<()> {
    let path = config::resolve_path(overrides);
    match cmd {
        ConfigCmd::Path => println!("{}", path.display()),
        ConfigCmd::Init { force } => {
            let url = overrides
                .url
                .clone()
                .or_else(|| std::env::var(config::env::URL).ok())
                .unwrap_or_else(|| config::DEFAULT_DEGOOG_URL.into());
            config::write_template(&path, url.trim_end_matches('/'), force)?;
            println!("wrote {}", path.display());
            if url == config::DEFAULT_DEGOOG_URL {
                println!("edit degoog.url to point at your Degoog instance, then run `degoog health`");
            }
        }
        ConfigCmd::Show => {
            let loaded = config::load(overrides);
            for w in &loaded.warnings {
                eprintln!("warning: {w}");
            }
            let mut shown = loaded.config.clone();
            shown.degoog.api_key = text::mask_secret(&shown.degoog.api_key);
            println!("# {}{}", loaded.path.display(), if loaded.file_found { "" } else { " (not found, defaults)" });
            print!("{}", toml::to_string_pretty(&shown)?);
        }
    }
    Ok(())
}

fn build_ctx(global: &Global, overrides: &Overrides, human: bool) -> anyhow::Result<Ctx> {
    let loaded = config::load(overrides);
    for w in &loaded.warnings {
        degoog_cli::warn!("config", "{w}");
    }
    let cfg = loaded.config;
    let cache = if global.no_cache {
        Cache::new(CacheConfig { enabled: false, ..cfg.cache.clone() })
    } else if cfg.cache.persist && cfg.cache.enabled {
        match config::default_cache_path() {
            Some(path) => Cache::persistent(cfg.cache.clone(), path),
            None => Cache::new(cfg.cache.clone()),
        }
    } else {
        Cache::new(cfg.cache.clone())
    };
    Ctx::new(cfg, cache, loaded.path, loaded.file_found, human).context("could not set up HTTP clients")
}

fn joined(words: &[String]) -> String {
    words.join(" ")
}

async fn run(cli: Cli) -> anyhow::Result<ExitCode> {
    let global = cli.global;
    set_log_level(&global);
    let overrides = Overrides { config_path: global.config.clone(), url: global.url.clone(), api_key: global.api_key.clone() };

    let command = match cli.command {
        Cmd::Config(cmd) => {
            config_command(cmd, &overrides)?;
            return Ok(ExitCode::SUCCESS);
        }
        Cmd::Mcp => {
            let ctx = Arc::new(build_ctx(&global, &overrides, false)?);
            mcp::serve(ctx).await?;
            return Ok(ExitCode::SUCCESS);
        }
        other => other,
    };

    let ctx = build_ctx(&global, &overrides, !global.json)?;
    let result = match command {
        Cmd::Search(a) => {
            search::run_search(
                &ctx,
                search::SearchArgs {
                    query: joined(&a.query),
                    kind: a.kind,
                    page: a.page,
                    time: a.time,
                    lang: a.lang,
                    date_from: a.from,
                    date_to: a.to,
                    engines: a.engines,
                    safe_mode: a.safe,
                    max_results: a.max_results,
                },
            )
            .await
        }
        Cmd::Scrape(a) => {
            scrape::run(&ctx, scrape::ScrapeArgs { urls: a.urls, query: a.query, max_chars: a.max_chars, max_chunks: a.max_chunks })
                .await
        }
        Cmd::Bundle(a) => {
            let mut queries: Vec<String> = Vec::new();
            if !a.query.is_empty() {
                queries.push(joined(&a.query));
            }
            queries.extend(a.also);
            let expand = if a.expand { Some(true) } else if a.no_expand { Some(false) } else { None };
            bundle::run(
                &ctx,
                bundle::BundleArgs {
                    query: None,
                    queries: Some(queries),
                    kind: a.kind,
                    lang: a.lang,
                    time: a.time,
                    expand_queries: expand,
                    max_scrape_urls: a.sources,
                    max_evidence_chars: a.max_chars,
                },
            )
            .await
        }
        Cmd::Discover => info::run_discover(&ctx).await,
        Cmd::Retry(a) => {
            search::run_retry(
                &ctx,
                search::RetryArgs { query: joined(&a.query), engine: a.engine, kind: a.kind, page: a.page, lang: a.lang },
            )
            .await
        }
        Cmd::Command(a) => info::run_command(&ctx, info::CommandArgs { query: joined(&a.query), page: a.page }).await,
        Cmd::Health => info::run_health(&ctx).await,
        Cmd::Mcp | Cmd::Config(_) => unreachable!(),
    };
    ctx.cache.save();
    Ok(print_result(&result, global.json))
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    let runtime = match tokio::runtime::Builder::new_multi_thread().enable_all().build() {
        Ok(rt) => rt,
        Err(err) => {
            eprintln!("error: could not start the async runtime: {err}");
            return ExitCode::FAILURE;
        }
    };
    match runtime.block_on(run(cli)) {
        Ok(code) => code,
        Err(err) => {
            eprintln!("error: {err:#}");
            ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::CommandFactory;

    #[test]
    fn cli_definition_is_consistent() {
        Cli::command().debug_assert();
    }
}
