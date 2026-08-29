use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command as ProcessCommand, ExitCode, Output};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, anyhow, bail};
use clap::{Parser, Subcommand};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

const PROFILE_SCHEMA_VERSION: &str = "1";
const PROVIDER_PROTOCOL: &str = "okf-provider/1";
const DEFAULT_LIBRARY_ID: &str = "project-context";
const LIBRARY_DIR: &str = ".okf/project-context";

#[derive(Parser, Debug)]
#[command(name = "project-context", version, about)]
struct Cli {
    #[command(subcommand)]
    command: Command,
    /// Emit JSON for application commands. Provider responses are always protocol JSON.
    #[arg(long, global = true)]
    json: bool,
}

#[derive(Subcommand, Debug)]
enum Command {
    /// Initialize a Project Context Library in a Git repository.
    Init {
        #[arg(long, default_value = ".")]
        repository: PathBuf,
        #[arg(long)]
        project: Option<String>,
        #[arg(long, default_value = DEFAULT_LIBRARY_ID)]
        id: String,
        #[arg(long)]
        force: bool,
        /// Also install and mount the generated Library with the `okf` CLI.
        #[arg(long)]
        mount: bool,
    },
    /// Report Project Context freshness and incrementally impacted topics.
    Status {
        #[arg(long, default_value = ".")]
        repository: PathBuf,
    },
    /// Mark the current clean repository HEAD as validated.
    Checkpoint {
        #[arg(long, default_value = ".")]
        repository: PathBuf,
        /// Explicit revision. It must resolve to the current HEAD.
        #[arg(long)]
        revision: Option<String>,
    },
    /// Install/update and mount the generated Library into the repository-local OKF registry.
    Mount {
        #[arg(long, default_value = ".")]
        repository: PathBuf,
    },
    /// Serve one `okf-provider/1` request on stdin/stdout.
    Provider {
        #[arg(long)]
        library_root: Option<PathBuf>,
    },
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct ImpactRule {
    topic: String,
    path_prefixes: Vec<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct Profile {
    schema_version: String,
    project: String,
    library_id: String,
    repository: PathBuf,
    validated_revision: Option<String>,
    impact_rules: Vec<ImpactRule>,
    #[serde(default)]
    excluded_prefixes: Vec<String>,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
enum ContextState {
    Uninitialized,
    Valid,
    Dirty,
    Unknown,
}

impl std::fmt::Display for ContextState {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::Uninitialized => "UNINITIALIZED",
            Self::Valid => "VALID",
            Self::Dirty => "DIRTY",
            Self::Unknown => "UNKNOWN",
        })
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct ContextStatus {
    state: ContextState,
    project: String,
    library_id: String,
    repository: PathBuf,
    validated_revision: Option<String>,
    current_revision: Option<String>,
    branch: Option<String>,
    changed_paths: Vec<String>,
    impacted_topics: Vec<String>,
}

#[derive(Clone, Debug, Deserialize)]
struct ProviderRequest {
    protocol: String,
    operation: String,
    library: String,
    #[serde(default)]
    path: Option<String>,
    #[serde(default)]
    uri: Option<String>,
    #[serde(default)]
    query: Option<ProviderQuery>,
}

#[derive(Clone, Debug, Deserialize)]
struct ProviderQuery {
    text: String,
    #[serde(default = "default_query_limit")]
    limit: usize,
}

#[derive(Clone, Debug, Serialize)]
struct CatalogEntry {
    id: String,
    title: String,
    description: Option<String>,
    uri: String,
    terms: BTreeSet<String>,
}

#[derive(Clone, Debug, Serialize)]
struct LibraryCatalog {
    library: String,
    entries: Vec<CatalogEntry>,
}

#[derive(Clone, Debug, Serialize)]
struct KnowledgeNode {
    uri: String,
    kind: String,
    title: Option<String>,
    virtual_node: bool,
}

#[derive(Clone, Debug, Serialize)]
struct QueryHit {
    uri: String,
    title: Option<String>,
    snippet: Option<String>,
    score: Option<f64>,
    metadata: BTreeMap<String, String>,
}

#[derive(Clone, Debug, Serialize)]
struct QueryResult {
    answer: Option<String>,
    hits: Vec<QueryHit>,
    provider: String,
    strategy: String,
    provenance: BTreeMap<String, String>,
}

fn main() -> ExitCode {
    match run(Cli::parse()) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("error: {error:#}");
            ExitCode::from(1)
        }
    }
}

fn run(cli: Cli) -> Result<()> {
    match cli.command {
        Command::Init {
            repository,
            project,
            id,
            force,
            mount,
        } => emit(cli.json, &init(&repository, project.as_deref(), &id, force, mount)?)?,
        Command::Status { repository } => emit(cli.json, &status_for_repository(&repository)?)?,
        Command::Checkpoint {
            repository,
            revision,
        } => emit(cli.json, &checkpoint(&repository, revision.as_deref())?)?,
        Command::Mount { repository } => emit(cli.json, &mount_library(&repository)?)?,
        Command::Provider { library_root } => serve_provider(library_root.as_deref())?,
    }
    Ok(())
}

fn emit<T: Serialize + std::fmt::Debug>(json_output: bool, value: &T) -> Result<()> {
    if json_output {
        println!("{}", serde_json::to_string_pretty(value)?);
    } else {
        println!("{value:#?}");
    }
    Ok(())
}

fn init(
    repository: &Path,
    project: Option<&str>,
    id: &str,
    force: bool,
    mount: bool,
) -> Result<ContextStatus> {
    validate_library_id(id)?;
    let repository = canonical_repository(repository)?;
    ensure_git_repository(&repository)?;
    let root = library_root(&repository);
    if root.exists() && !force {
        bail!("Project Context Library already exists at {}", root.display());
    }
    if root.exists() {
        fs::remove_dir_all(&root)
            .with_context(|| format!("failed to reset {}", root.display()))?;
    }
    let project = project
        .map(str::to_owned)
        .or_else(|| {
            repository
                .file_name()
                .map(|value| value.to_string_lossy().into_owned())
        })
        .unwrap_or_else(|| id.to_owned());
    create_scaffold(&root, id, &project)?;
    let profile = Profile {
        schema_version: PROFILE_SCHEMA_VERSION.to_owned(),
        project,
        library_id: id.to_owned(),
        repository: repository.clone(),
        validated_revision: None,
        impact_rules: default_impact_rules(id),
        excluded_prefixes: vec![
            LIBRARY_DIR.to_owned(),
            ".okf/cache".to_owned(),
            ".okf/libraries.json".to_owned(),
        ],
    };
    save_profile(&root, &profile)?;
    if mount {
        mount_library(&repository)?;
    }
    compute_status(&profile)
}

fn status_for_repository(repository: &Path) -> Result<ContextStatus> {
    let repository = canonical_repository(repository)?;
    ensure_git_repository(&repository)?;
    let root = library_root(&repository);
    if !profile_path(&root).is_file() {
        return Ok(ContextStatus {
            state: ContextState::Uninitialized,
            project: repository.file_name().map_or_else(
                || "project".to_owned(),
                |value| value.to_string_lossy().into_owned(),
            ),
            library_id: DEFAULT_LIBRARY_ID.to_owned(),
            repository: repository.clone(),
            validated_revision: None,
            current_revision: current_revision(&repository).ok(),
            branch: current_branch(&repository),
            changed_paths: working_tree_paths(&repository, &[LIBRARY_DIR.to_owned()])
                .unwrap_or_default(),
            impacted_topics: Vec::new(),
        });
    }
    compute_status(&load_profile(&root)?)
}

fn checkpoint(repository: &Path, revision: Option<&str>) -> Result<ContextStatus> {
    let repository = canonical_repository(repository)?;
    let root = library_root(&repository);
    let mut profile = load_profile(&root)?;
    let working_paths = working_tree_paths(&profile.repository, &profile.excluded_prefixes)?;
    if !working_paths.is_empty() {
        bail!(
            "refusing to checkpoint a dirty working tree; commit or revert these paths first: {}",
            working_paths.join(", ")
        );
    }

    let current = current_revision(&profile.repository)?;
    let requested = revision.unwrap_or(&current);
    let resolved = resolve_revision(&profile.repository, requested)?;
    if resolved != current {
        bail!(
            "checkpoint revision '{requested}' resolves to {resolved}, but current HEAD is {current}; checkpoint the current verified HEAD"
        );
    }

    profile.validated_revision = Some(current.clone());
    save_profile(&root, &profile)?;
    append_history(&root, &current)?;
    compute_status(&profile)
}

fn mount_library(repository: &Path) -> Result<ContextStatus> {
    let repository = canonical_repository(repository)?;
    let root = library_root(&repository);
    let profile = load_profile(&root)?;
    let registry = repository.join(".okf/libraries.json");

    let add = ProcessCommand::new("okf")
        .arg("--registry")
        .arg(&registry)
        .args(["library", "add"])
        .arg(&root)
        .args(["--id", &profile.library_id, "--name"])
        .arg(format!("{} Project Context", profile.project))
        .output()
        .context("failed to execute the okf CLI; install OKF CLI 0.2 or newer")?;
    if !add.status.success() {
        let message = String::from_utf8_lossy(&add.stderr);
        if message.contains("already installed") {
            run_okf(&registry, &["library", "update", &profile.library_id])?;
        } else {
            bail!("okf library add failed: {}", message.trim());
        }
    }
    run_okf(
        &registry,
        &[
            "library",
            "mount",
            &profile.library_id,
            "--allow-provider",
            "process",
        ],
    )?;
    compute_status(&profile)
}

fn serve_provider(explicit_root: Option<&Path>) -> Result<()> {
    let root = explicit_root
        .map(Path::to_path_buf)
        .or_else(|| std::env::var_os("OKF_LIBRARY_ROOT").map(PathBuf::from))
        .ok_or_else(|| anyhow!("provider requires --library-root or OKF_LIBRARY_ROOT"))?;
    let root = root.canonicalize().unwrap_or(root);
    let profile = load_profile(&root)?;
    let mut input = Vec::new();
    std::io::stdin().read_to_end(&mut input)?;
    let request: ProviderRequest =
        serde_json::from_slice(&input).context("failed to parse okf-provider/1 request")?;
    let response = match handle_provider_request(&root, &profile, request) {
        Ok(data) => json!({"ok": true, "data": data}),
        Err(error) => json!({
            "ok": false,
            "error": {"code": "project-context-error", "message": format!("{error:#}")}
        }),
    };
    let mut stdout = std::io::stdout().lock();
    serde_json::to_writer(&mut stdout, &response)?;
    stdout.write_all(b"\n")?;
    Ok(())
}

fn handle_provider_request(root: &Path, profile: &Profile, request: ProviderRequest) -> Result<Value> {
    if request.protocol != PROVIDER_PROTOCOL {
        bail!("unsupported provider protocol '{}'", request.protocol);
    }
    if request.library != profile.library_id {
        bail!(
            "provider belongs to Library '{}' but request targets '{}'",
            profile.library_id,
            request.library
        );
    }
    match request.operation.as_str() {
        "catalog" => Ok(serde_json::to_value(build_catalog(profile))?),
        "list" => Ok(serde_json::to_value(list_nodes(
            profile,
            request.path.as_deref().unwrap_or(""),
        )?)?),
        "read" => {
            let uri = request.uri.ok_or_else(|| anyhow!("read requires uri"))?;
            let path = uri_path(&uri, &profile.library_id)?;
            Ok(Value::String(read_node(root, profile, &path)?))
        }
        "query" => {
            let query = request
                .query
                .ok_or_else(|| anyhow!("query requires query payload"))?;
            Ok(serde_json::to_value(query_library(root, profile, &query)?)?)
        }
        "refresh" => {
            let _ = compute_status(profile)?;
            Ok(Value::Null)
        }
        other => bail!("unsupported provider operation '{other}'"),
    }
}

fn build_catalog(profile: &Profile) -> LibraryCatalog {
    let id = &profile.library_id;
    let mut entries = vec![
        catalog_entry(id, "status", "Live project freshness", "Current Git revision, working-tree freshness, and impacted topics.", "status", &["status", "freshness", "revision", "dirty"]),
        catalog_entry(id, "architecture", "Architecture", "Current architecture, boundaries, dependencies, and major flows.", "current/architecture", &["architecture", "modules", "boundaries"]),
        catalog_entry(id, "constraints", "Constraints", "Durable product and technical constraints.", "current/constraints", &["constraints", "invariants", "rules"]),
        catalog_entry(id, "decisions", "Decisions", "Active decisions, rationale, and supersession notes.", "current/decisions", &["decisions", "adr", "rationale"]),
        catalog_entry(id, "components", "Components", "Current component responsibilities and ownership boundaries.", "current/components", &["components", "modules", "packages"]),
        catalog_entry(id, "history", "Project history", "Append-only material context changes and validated checkpoints.", "history/log", &["history", "changes", "checkpoint"]),
    ];
    entries.sort_by(|left, right| left.id.cmp(&right.id));
    LibraryCatalog {
        library: id.clone(),
        entries,
    }
}

fn catalog_entry(
    library: &str,
    id: &str,
    title: &str,
    description: &str,
    path: &str,
    terms: &[&str],
) -> CatalogEntry {
    CatalogEntry {
        id: id.to_owned(),
        title: title.to_owned(),
        description: Some(description.to_owned()),
        uri: format!("okf://{library}/{path}"),
        terms: terms.iter().map(|value| (*value).to_owned()).collect(),
    }
}

fn list_nodes(profile: &Profile, path: &str) -> Result<Vec<KnowledgeNode>> {
    let path = normalize_knowledge_path(path)?;
    let id = &profile.library_id;
    let nodes = match path.as_str() {
        "" => vec![
            node(id, "index", "content", Some("Project Context"), false),
            node(id, "status", "content", Some("Live project freshness"), true),
            node(id, "current", "directory", Some("Current project knowledge"), false),
            node(id, "history", "directory", Some("Project history"), false),
        ],
        "current" => vec![
            node(id, "current/architecture", "content", Some("Architecture"), false),
            node(id, "current/constraints", "content", Some("Constraints"), false),
            node(id, "current/decisions", "content", Some("Decisions"), false),
            node(id, "current/components", "content", Some("Components"), false),
        ],
        "history" => vec![node(id, "history/log", "content", Some("Project history"), false)],
        _ => Vec::new(),
    };
    Ok(nodes)
}

fn node(
    library: &str,
    path: &str,
    kind: &str,
    title: Option<&str>,
    virtual_node: bool,
) -> KnowledgeNode {
    KnowledgeNode {
        uri: format!("okf://{library}/{path}"),
        kind: kind.to_owned(),
        title: title.map(str::to_owned),
        virtual_node,
    }
}

fn read_node(root: &Path, profile: &Profile, path: &str) -> Result<String> {
    let path = normalize_knowledge_path(path)?;
    if path == "status" {
        return Ok(status_markdown(&compute_status(profile)?));
    }
    let relative = match path.as_str() {
        "index" => "index.md",
        "current/architecture" => "current/architecture.md",
        "current/constraints" => "current/constraints.md",
        "current/decisions" => "current/decisions.md",
        "current/components" => "current/components.md",
        "history/log" => "history/log.md",
        _ => bail!("knowledge path '{path}' was not found"),
    };
    let file = root.join(relative);
    fs::read_to_string(&file).with_context(|| format!("failed to read {}", file.display()))
}

fn query_library(root: &Path, profile: &Profile, query: &ProviderQuery) -> Result<QueryResult> {
    let needle = query.text.trim().to_lowercase();
    let candidates = [
        ("status", "Live project freshness"),
        ("current/architecture", "Architecture"),
        ("current/constraints", "Constraints"),
        ("current/decisions", "Decisions"),
        ("current/components", "Components"),
        ("history/log", "Project history"),
    ];
    let mut hits = Vec::new();
    for (path, title) in candidates {
        let content = read_node(root, profile, path)?;
        let path_match = path.to_lowercase().contains(&needle);
        let title_match = title.to_lowercase().contains(&needle);
        let content_match = content.to_lowercase().contains(&needle);
        if !needle.is_empty() && !(path_match || title_match || content_match) {
            continue;
        }
        let score = if needle.is_empty() {
            0.0
        } else if path_match {
            3.0
        } else if title_match {
            2.0
        } else {
            1.0
        };
        hits.push(QueryHit {
            uri: format!("okf://{}/{path}", profile.library_id),
            title: Some(title.to_owned()),
            snippet: Some(truncate(&content, 240)),
            score: Some(score),
            metadata: BTreeMap::new(),
        });
    }
    hits.sort_by(|left, right| {
        right
            .score
            .partial_cmp(&left.score)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| left.uri.cmp(&right.uri))
    });
    hits.truncate(query.limit.min(1000));
    Ok(QueryResult {
        answer: None,
        hits,
        provider: "project-context".to_owned(),
        strategy: "lexical".to_owned(),
        provenance: BTreeMap::from([(
            "freshness".to_owned(),
            compute_status(profile)?.state.to_string(),
        )]),
    })
}

fn compute_status(profile: &Profile) -> Result<ContextStatus> {
    if profile.schema_version != PROFILE_SCHEMA_VERSION {
        bail!("unsupported profile schema '{}'", profile.schema_version);
    }
    let current = current_revision(&profile.repository).ok();
    let branch = current_branch(&profile.repository);
    let working =
        working_tree_paths(&profile.repository, &profile.excluded_prefixes).unwrap_or_default();
    let (state, changed_paths) = match (&profile.validated_revision, &current) {
        (None, _) => (ContextState::Uninitialized, working),
        (Some(_), None) => (ContextState::Unknown, working),
        (Some(validated), Some(current)) if validated == current => {
            if working.is_empty() {
                (ContextState::Valid, Vec::new())
            } else {
                (ContextState::Dirty, working)
            }
        }
        (Some(validated), Some(current)) => match changed_paths(&profile.repository, validated, current)
        {
            Ok(committed) => (
                ContextState::Dirty,
                merge_paths(filter_paths(committed, &profile.excluded_prefixes), working),
            ),
            Err(_) => (ContextState::Unknown, working),
        },
    };
    let impacted_topics = impacted_topics(
        &changed_paths,
        &profile.impact_rules,
        &profile.library_id,
    );
    Ok(ContextStatus {
        state,
        project: profile.project.clone(),
        library_id: profile.library_id.clone(),
        repository: profile.repository.clone(),
        validated_revision: profile.validated_revision.clone(),
        current_revision: current,
        branch,
        changed_paths,
        impacted_topics,
    })
}

fn create_scaffold(root: &Path, id: &str, project: &str) -> Result<()> {
    fs::create_dir_all(root.join("current"))?;
    fs::create_dir_all(root.join("history"))?;
    fs::write(
        root.join("okf-library.yaml"),
        format!(
            "schema_version: \"1\"\nid: {id}\nname: {project} Project Context\nversion: \"0.1\"\n\ncatalog:\n  - id: status\n    title: Live project freshness\n    description: Current Git revision, working-tree freshness, and impacted topics.\n    path: status\n    terms: [status, freshness, revision, dirty]\n  - id: architecture\n    title: Architecture\n    path: current/architecture\n    terms: [architecture, modules, boundaries]\n  - id: constraints\n    title: Constraints\n    path: current/constraints\n    terms: [constraints, invariants, rules]\n  - id: decisions\n    title: Decisions\n    path: current/decisions\n    terms: [decisions, adr, rationale]\n  - id: components\n    title: Components\n    path: current/components\n    terms: [components, modules, packages]\n  - id: history\n    title: Project history\n    path: history/log\n    terms: [history, changes, checkpoints]\n\nquery:\n  preferred: lexical\n  capabilities: [lexical]\n  hints:\n    - Read status before broad repository exploration.\n    - Prefer current/ for present-tense project understanding.\n    - Use history/log for why and when the project changed.\n\nproviders:\n  - id: project-context-runtime\n    kind: process\n    capabilities: [catalog, list, read, query, refresh]\n    config:\n      command: project-context\n      args: [provider, --library-root, \"${{library_root}}\"]\n"
        ),
    )?;
    write_doc(
        &root.join("index.md"),
        "Project Context",
        "Entry point for durable, revision-aware project knowledge.",
    )?;
    write_doc(
        &root.join("current/architecture.md"),
        "Architecture",
        "Current architecture, boundaries, dependencies, and major flows.",
    )?;
    write_doc(
        &root.join("current/constraints.md"),
        "Constraints",
        "Current invariants, compatibility requirements, and non-negotiable constraints.",
    )?;
    write_doc(
        &root.join("current/decisions.md"),
        "Decisions",
        "Active decisions with rationale and supersession notes.",
    )?;
    write_doc(
        &root.join("current/components.md"),
        "Components",
        "Current component responsibilities, interfaces, and ownership boundaries.",
    )?;
    write_doc(
        &root.join("history/log.md"),
        "Project History",
        "Append-only history of material context changes and validated checkpoints.",
    )?;
    Ok(())
}

fn write_doc(path: &Path, title: &str, summary: &str) -> Result<()> {
    fs::write(
        path,
        format!(
            "---\ntitle: {title}\nsummary: {summary}\ntags: [project-context]\n---\n# {title}\n\n<!-- Maintained by an authorized Project Context workflow. -->\n"
        ),
    )
    .with_context(|| format!("failed to write {}", path.display()))
}

fn default_impact_rules(id: &str) -> Vec<ImpactRule> {
    vec![
        ImpactRule {
            topic: format!("okf://{id}/current/architecture"),
            path_prefixes: vec!["src".into(), "packages".into(), "crates".into(), "docs".into()],
        },
        ImpactRule {
            topic: format!("okf://{id}/current/components"),
            path_prefixes: vec!["src".into(), "packages".into(), "crates".into()],
        },
        ImpactRule {
            topic: format!("okf://{id}/current/constraints"),
            path_prefixes: vec![
                ".github".into(),
                "Cargo.toml".into(),
                "package.json".into(),
                "pom.xml".into(),
                "build.gradle".into(),
                "go.mod".into(),
                "pyproject.toml".into(),
            ],
        },
        ImpactRule {
            topic: format!("okf://{id}/current/decisions"),
            path_prefixes: vec!["docs".into(), "adr".into(), "decisions".into()],
        },
    ]
}

fn impacted_topics(changed: &[String], rules: &[ImpactRule], library_id: &str) -> Vec<String> {
    if changed.is_empty() {
        return Vec::new();
    }
    let mut topics = BTreeSet::new();
    for rule in rules {
        if rule.path_prefixes.iter().any(|prefix| {
            changed
                .iter()
                .any(|path| path_matches_prefix(path, prefix))
        }) {
            topics.insert(rule.topic.clone());
        }
    }
    if topics.is_empty() {
        topics.extend(all_current_topics(library_id));
    }
    topics.into_iter().collect()
}

fn all_current_topics(id: &str) -> Vec<String> {
    ["architecture", "components", "constraints", "decisions"]
        .into_iter()
        .map(|topic| format!("okf://{id}/current/{topic}"))
        .collect()
}

fn status_markdown(status: &ContextStatus) -> String {
    let changed = if status.changed_paths.is_empty() {
        "none".to_owned()
    } else {
        status.changed_paths.join(", ")
    };
    let impacted = if status.impacted_topics.is_empty() {
        "none".to_owned()
    } else {
        status.impacted_topics.join(", ")
    };
    format!(
        "# Project Context Status\n\n- state: {}\n- project: {}\n- branch: {}\n- validated revision: {}\n- current revision: {}\n- changed paths: {}\n- impacted topics: {}\n",
        status.state,
        status.project,
        status.branch.as_deref().unwrap_or("<detached-or-unknown>"),
        status.validated_revision.as_deref().unwrap_or("<none>"),
        status.current_revision.as_deref().unwrap_or("<unknown>"),
        changed,
        impacted
    )
}

fn append_history(root: &Path, revision: &str) -> Result<()> {
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|error| anyhow!("system clock is before Unix epoch: {error}"))?
        .as_secs();
    let path = root.join("history/log.md");
    let mut content = fs::read_to_string(&path).unwrap_or_default();
    content.push_str(&format!(
        "\n## Checkpoint {revision}\n\n- unix_time: {timestamp}\n- validated_revision: {revision}\n"
    ));
    fs::write(&path, content).with_context(|| format!("failed to write {}", path.display()))
}

fn canonical_repository(path: &Path) -> Result<PathBuf> {
    path.canonicalize()
        .with_context(|| format!("failed to resolve repository {}", path.display()))
}

fn ensure_git_repository(repository: &Path) -> Result<()> {
    current_revision(repository).map(|_| ())
}

fn current_revision(repository: &Path) -> Result<String> {
    git_output(repository, &["rev-parse", "HEAD"])
}

fn current_branch(repository: &Path) -> Option<String> {
    git_output(repository, &["symbolic-ref", "--short", "-q", "HEAD"])
        .ok()
        .filter(|value| !value.is_empty())
}

fn resolve_revision(repository: &Path, revision: &str) -> Result<String> {
    git_output(
        repository,
        &["rev-parse", "--verify", &format!("{revision}^{{commit}}")],
    )
}

fn changed_paths(repository: &Path, from: &str, to: &str) -> Result<Vec<String>> {
    let output = git_output(
        repository,
        &["diff", "--name-only", &format!("{from}..{to}")],
    )?;
    Ok(lines(&output))
}

fn working_tree_paths(repository: &Path, excluded: &[String]) -> Result<Vec<String>> {
    let mut paths = BTreeSet::new();
    for args in [
        &["diff", "--name-only"][..],
        &["diff", "--cached", "--name-only"][..],
        &["ls-files", "--others", "--exclude-standard"][..],
    ] {
        paths.extend(lines(&git_output(repository, args)?));
    }
    Ok(filter_paths(paths.into_iter().collect(), excluded))
}

fn lines(value: &str) -> Vec<String> {
    value
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(normalize_repo_path)
        .collect()
}

fn merge_paths(left: Vec<String>, right: Vec<String>) -> Vec<String> {
    left.into_iter()
        .chain(right)
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect()
}

fn filter_paths(paths: Vec<String>, excluded: &[String]) -> Vec<String> {
    paths
        .into_iter()
        .filter(|path| !excluded.iter().any(|prefix| path_matches_prefix(path, prefix)))
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect()
}

fn path_matches_prefix(path: &str, prefix: &str) -> bool {
    let path = normalize_repo_path(path);
    let prefix = normalize_repo_path(prefix);
    prefix.is_empty()
        || path == prefix
        || path
            .strip_prefix(&prefix)
            .is_some_and(|suffix| suffix.starts_with('/'))
}

fn normalize_repo_path(value: &str) -> String {
    value
        .trim()
        .replace('\\', "/")
        .trim_start_matches("./")
        .trim_matches('/')
        .to_owned()
}

fn git_output(repository: &Path, args: &[&str]) -> Result<String> {
    let output = ProcessCommand::new("git")
        .arg("-C")
        .arg(repository)
        .args(args)
        .output()
        .with_context(|| format!("failed to execute git {}", args.join(" ")))?;
    if !output.status.success() {
        bail!(
            "git {} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    Ok(String::from_utf8(output.stdout)
        .map_err(|error| anyhow!("git output was not UTF-8: {error}"))?
        .trim()
        .to_owned())
}

fn run_okf(registry: &Path, args: &[&str]) -> Result<()> {
    let output = ProcessCommand::new("okf")
        .arg("--registry")
        .arg(registry)
        .args(args)
        .output()
        .with_context(|| format!("failed to execute okf {}", args.join(" ")))?;
    ensure_success("okf", args, &output)
}

fn ensure_success(program: &str, args: &[&str], output: &Output) -> Result<()> {
    if output.status.success() {
        Ok(())
    } else {
        bail!(
            "{program} {} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&output.stderr).trim()
        )
    }
}

fn library_root(repository: &Path) -> PathBuf {
    repository.join(LIBRARY_DIR)
}

fn profile_path(root: &Path) -> PathBuf {
    root.join("profile.json")
}

fn load_profile(root: &Path) -> Result<Profile> {
    let path = profile_path(root);
    let bytes = fs::read(&path).with_context(|| format!("failed to read {}", path.display()))?;
    let profile: Profile = serde_json::from_slice(&bytes)
        .with_context(|| format!("failed to parse {}", path.display()))?;
    if profile.schema_version != PROFILE_SCHEMA_VERSION {
        bail!(
            "unsupported Project Context profile schema version '{}'",
            profile.schema_version
        );
    }
    Ok(profile)
}

fn save_profile(root: &Path, profile: &Profile) -> Result<()> {
    fs::create_dir_all(root)?;
    let path = profile_path(root);
    let mut bytes = serde_json::to_vec_pretty(profile)?;
    bytes.push(b'\n');
    fs::write(&path, bytes).with_context(|| format!("failed to write {}", path.display()))
}

fn uri_path(uri: &str, library: &str) -> Result<String> {
    let prefix = format!("okf://{library}/");
    let path = uri
        .strip_prefix(&prefix)
        .ok_or_else(|| anyhow!("URI '{uri}' does not belong to Library '{library}'"))?;
    normalize_knowledge_path(path)
}

fn normalize_knowledge_path(value: &str) -> Result<String> {
    if value.contains('\\') || value.contains('\0') {
        bail!("invalid knowledge path");
    }
    let trimmed = value.trim().trim_matches('/');
    let mut segments = Vec::new();
    for segment in trimmed.split('/') {
        if segment.is_empty() {
            continue;
        }
        if segment == "." || segment == ".." || segment.contains(':') {
            bail!("invalid knowledge path segment '{segment}'");
        }
        segments.push(segment);
    }
    Ok(segments.join("/"))
}

fn validate_library_id(value: &str) -> Result<()> {
    if value.is_empty()
        || !value.chars().all(|character| {
            character.is_ascii_alphanumeric() || matches!(character, '-' | '_' | '.')
        })
    {
        bail!("invalid Library id '{value}'");
    }
    Ok(())
}

fn truncate(value: &str, max_chars: usize) -> String {
    let mut chars = value.chars();
    let prefix = chars.by_ref().take(max_chars).collect::<String>();
    if chars.next().is_some() {
        format!("{prefix}…")
    } else {
        prefix
    }
}

fn default_query_limit() -> usize {
    20
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_knowledge_path_traversal() {
        assert!(normalize_knowledge_path("../secret").is_err());
        assert!(normalize_knowledge_path("current\\secret").is_err());
        assert!(uri_path("okf://demo/../../secret", "demo").is_err());
    }

    #[test]
    fn impact_analysis_is_conservative_for_unknown_paths() {
        let topics = impacted_topics(
            &["new-layout/module.xyz".to_owned()],
            &default_impact_rules("demo"),
            "demo",
        );
        assert_eq!(topics, all_current_topics("demo"));
    }

    #[test]
    fn prefix_matching_respects_path_segments() {
        assert!(path_matches_prefix("src/lib.rs", "src"));
        assert!(path_matches_prefix("src", "src"));
        assert!(!path_matches_prefix("src2/lib.rs", "src"));
    }

    #[test]
    fn library_ids_are_portable() {
        assert!(validate_library_id("project-context.v1").is_ok());
        assert!(validate_library_id("../project").is_err());
    }
}
