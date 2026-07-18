use std::{hint::black_box, time::Duration};

use criterion::{BenchmarkId, Criterion, criterion_group, criterion_main};
use memzoi_core::{
    ContextPackInput, InitRequest, MemoryDestination, MemoryLane, MemoryService, MemoryStatus,
    MemoryType, OriginDescriptor, OriginRoute, PrecheckInput, RetentionFacts, ScopeKind,
    SearchInput, Visibility,
};
use rusqlite::{Connection, params};
use tempfile::TempDir;

fn bench_search_memory(c: &mut Criterion) {
    let mut group = c.benchmark_group("search_memory");
    for size in [10usize, 100, 1_000, 10_000] {
        let fixture = BenchFixture::with_corpus(size, size / 20).expect("seed benchmark corpus");
        let input = SearchInput {
            query: "routing recall token".to_owned(),
            limit: 10,
            include_inactive: false,
            ..SearchInput::default()
        };
        group.bench_with_input(
            BenchmarkId::new("unfiltered_query", size),
            &input,
            |bench, input| {
                bench.iter(|| {
                    fixture
                        .service
                        .search_memory_for_benchmark(black_box(input.clone()))
                        .expect("search benchmark should succeed")
                });
            },
        );

        let filtered = SearchInput {
            query: "routing recall token".to_owned(),
            scope_kind: Some(ScopeKind::Repo),
            memory_type: Some(MemoryType::Decision),
            path_prefix: Some("crates/memzoi-core/src".to_owned()),
            limit: 10,
            include_inactive: false,
            ..SearchInput::default()
        };
        group.bench_with_input(
            BenchmarkId::new("type_scope_path_filtered", size),
            &filtered,
            |bench, input| {
                bench.iter(|| {
                    fixture
                        .service
                        .search_memory_for_benchmark(black_box(input.clone()))
                        .expect("filtered search benchmark should succeed")
                });
            },
        );
    }
    group.finish();
}

fn bench_context_pack(c: &mut Criterion) {
    let mut group = c.benchmark_group("context_pack");
    for size in [10usize, 100, 1_000, 10_000] {
        let fixture = BenchFixture::with_corpus(size, size / 20).expect("seed benchmark corpus");
        for budget in [160usize, 400, 1_600] {
            let input = ContextPackInput {
                task: "Implement routing recall token for Rust context packs".to_owned(),
                path_prefix: Some("crates/memzoi-core/src/context.rs".to_owned()),
                token_budget: Some(budget),
                include_local: false,
                include_session: false,
            };
            group.bench_with_input(
                BenchmarkId::new(format!("budget_{budget}"), size),
                &input,
                |bench, input| {
                    bench.iter(|| {
                        fixture
                            .service
                            .build_context_pack_for_benchmark(black_box(input.clone()))
                            .expect("context benchmark should succeed")
                    });
                },
            );
        }
    }
    group.finish();
}

fn bench_precheck(c: &mut Criterion) {
    let mut group = c.benchmark_group("precheck");
    for warning_density in [0usize, 1, 10, 100] {
        let fixture =
            BenchFixture::with_corpus(1_000, warning_density).expect("seed benchmark corpus");
        let path_action = PrecheckInput {
            path: Some("crates/memzoi-core/src/precheck.rs".to_owned()),
            action: Some("change risky precheck command handling".to_owned()),
            command: None,
            scope_kind: Some(ScopeKind::Repo),
        };
        group.bench_with_input(
            BenchmarkId::new("path_action", warning_density),
            &path_action,
            |bench, input| {
                bench.iter(|| {
                    fixture
                        .service
                        .precheck_for_benchmark(black_box(input.clone()))
                        .expect("precheck benchmark should succeed")
                });
            },
        );

        let command_only = PrecheckInput {
            path: None,
            action: None,
            command: Some("cargo clippy --workspace --all-targets -- -D warnings".to_owned()),
            scope_kind: Some(ScopeKind::Repo),
        };
        group.bench_with_input(
            BenchmarkId::new("command_only", warning_density),
            &command_only,
            |bench, input| {
                bench.iter(|| {
                    fixture
                        .service
                        .precheck_for_benchmark(black_box(input.clone()))
                        .expect("command precheck benchmark should succeed")
                });
            },
        );
    }
    group.finish();
}

struct BenchFixture {
    _temp: TempDir,
    service: MemoryService,
}

impl BenchFixture {
    fn with_corpus(records: usize, warning_density: usize) -> anyhow::Result<Self> {
        let temp = TempDir::new()?;
        let init = MemoryService::initialize(temp.path(), InitRequest { force: true })?;
        let conn = Connection::open(&init.paths.db_path)?;
        conn.pragma_update(None, "foreign_keys", "ON")?;

        insert_record(
            &conn,
            FixtureRecord {
                id: "target-routing-decision".to_owned(),
                memory_type: MemoryType::Decision,
                status: MemoryStatus::Active,
                title: "Routing recall token decision".to_owned(),
                body: "Use the routing recall token for Rust context packs and prioritize matching source paths.".to_owned(),
                path: "crates/memzoi-core/src/context.rs".to_owned(),
            },
        )?;
        insert_record(
            &conn,
            FixtureRecord {
                id: "target-precheck-risk".to_owned(),
                memory_type: MemoryType::Risk,
                status: MemoryStatus::Active,
                title: "Risky precheck command handling".to_owned(),
                body: "Changing risky precheck command handling previously hid destructive command warnings.".to_owned(),
                path: "crates/memzoi-core/src/precheck.rs".to_owned(),
            },
        )?;

        for index in 0..records {
            let is_warning = warning_density != 0 && index % warning_density == 0;
            let memory_type = if is_warning {
                MemoryType::Warning
            } else if index % 2 == 0 {
                MemoryType::Decision
            } else {
                MemoryType::Fact
            };
            let status = if index % 17 == 0 {
                MemoryStatus::Superseded
            } else {
                MemoryStatus::Active
            };
            let path = if index % 3 == 0 {
                format!("crates/memzoi-core/src/module_{index}.rs")
            } else {
                format!("apps/frontend/src/module_{index}.tsx")
            };
            insert_record(
                &conn,
                FixtureRecord {
                    id: format!("bench-distractor-{index}"),
                    memory_type,
                    status,
                    title: format!("Benchmark distractor memory {index}"),
                    body: format!(
                        "Benchmark body {index} mentions routing recall token and precheck handling for repeatable ranking noise."
                    ),
                    path,
                },
            )?;
        }
        drop(conn);

        let service = MemoryService::open(temp.path())?;
        Ok(Self {
            _temp: temp,
            service,
        })
    }
}

struct FixtureRecord {
    id: String,
    memory_type: MemoryType,
    status: MemoryStatus,
    title: String,
    body: String,
    path: String,
}

fn insert_record(conn: &Connection, record: FixtureRecord) -> anyhow::Result<()> {
    let retention = RetentionFacts {
        policy_version: "memzoi/lane-retention-v1".to_owned(),
        occurred_at: None,
        started_at: None,
        last_continued_at: None,
        closed_at: None,
        explicit_expires_at: None,
        episodic_extension: None,
    };
    let origin = OriginDescriptor::new(
        format!("benchmark:{}", record.id),
        OriginRoute::RepositoryMaterialization,
    );
    conn.execute(
        "INSERT INTO memory_record(
            id, type, lane, destination, scope_kind, visibility, title, body, status, confidence,
            source_kind, source_ref, content_hash, created_at, updated_at, retention_json,
            origin_json
         ) VALUES (
            ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, 0.91, 'bench', ?10, ?11,
            '2026-07-18T00:00:00Z', '2026-07-18T00:00:00Z', ?12, ?13
         )",
        params![
            record.id,
            record.memory_type.as_str(),
            MemoryLane::Semantic.as_str(),
            MemoryDestination::Repo.as_str(),
            ScopeKind::Repo.as_str(),
            Visibility::Repo.as_str(),
            record.title,
            record.body,
            record.status.as_str(),
            format!("bench://{}", record.path),
            format!("hash-{}", record.id),
            serde_json::to_string(&retention)?,
            serde_json::to_string(&origin)?,
        ],
    )?;
    conn.execute(
        "INSERT INTO memory_path(id, record_id, path, line_start, line_end)
         VALUES (?1, ?2, ?3, 1, 12)",
        params![format!("path-{}", record.id), record.id, record.path],
    )?;
    Ok(())
}

criterion_group! {
    name = benches;
    config = Criterion::default()
        .sample_size(10)
        .warm_up_time(Duration::from_millis(200))
        .measurement_time(Duration::from_secs(1));
    targets = bench_search_memory, bench_context_pack, bench_precheck
}
criterion_main!(benches);
