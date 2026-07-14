use std::{cell::Cell, fs, path::Path, time::Duration};

use criterion::{BatchSize, Criterion, Throughput, criterion_group, criterion_main};
use grimmored::{
    storage::Storage,
    vault_index::{index_vault, reconcile_vault_paths, search_notes},
};
use tempfile::TempDir;

const QUERY_NOTE_COUNT: usize = 2_000;
const QUERY_NOTE_BYTES: usize = 4 * 1024;
const INCREMENTAL_NOTE_BYTES: usize = 50 * 1024;

struct BenchVault {
    _workspace: TempDir,
    root: std::path::PathBuf,
    storage: Storage,
}

impl BenchVault {
    fn new(note_count: usize, note_bytes: usize) -> Self {
        let workspace = TempDir::new().expect("create benchmark workspace");
        let root = workspace.path().join("vault");
        create_corpus(&root, note_count, note_bytes);
        let storage = Storage::open(workspace.path().join("operational.sqlite3"))
            .expect("open benchmark SQLite database");
        index_vault(&storage, "benchmark", &root).expect("index benchmark vault");
        Self {
            _workspace: workspace,
            root,
            storage,
        }
    }
}

fn create_corpus(root: &Path, note_count: usize, note_bytes: usize) {
    for index in 0..note_count {
        let directory = root.join(format!("knowledge/{:03}", index / 250));
        fs::create_dir_all(&directory).expect("create benchmark corpus directory");
        let marker = (index == 0).then_some(" raregrimmoretoken ");
        fs::write(
            directory.join(format!("note-{index:05}.md")),
            note_body(index, note_bytes, marker),
        )
        .expect("write benchmark corpus note");
    }
}

fn note_body(index: usize, bytes: usize, marker: Option<&str>) -> String {
    let mut body = format!(
        "# Benchmark note {index}\n\n broadgrimmoretoken {}",
        marker.unwrap_or_default()
    );
    let filler = "local first knowledge evidence retrieval benchmark ";
    assert!(body.len() <= bytes, "benchmark note size is too small");
    while body.len() + filler.len() <= bytes {
        body.push_str(filler);
    }
    body.extend(std::iter::repeat_n('x', bytes - body.len()));
    body
}

fn benchmark_fts_query(criterion: &mut Criterion) {
    let fixture = BenchVault::new(QUERY_NOTE_COUNT, QUERY_NOTE_BYTES);
    let mut group = criterion.benchmark_group("vault_fts_top_20");
    group
        .sample_size(100)
        .warm_up_time(Duration::from_secs(1))
        .measurement_time(Duration::from_secs(3))
        .throughput(Throughput::Elements(QUERY_NOTE_COUNT as u64));
    for (name, query) in [
        ("rare_term_2000_notes", "raregrimmoretoken"),
        ("broad_term_2000_notes", "broadgrimmoretoken"),
    ] {
        group.bench_function(name, |bencher| {
            bencher.iter(|| {
                let result = search_notes(
                    std::hint::black_box(&fixture.storage),
                    "benchmark",
                    query,
                    20,
                )
                .expect("run bounded benchmark FTS query");
                std::hint::black_box(result);
            });
        });
    }
    group.finish();
}

fn benchmark_incremental_note(criterion: &mut Criterion) {
    let fixture = BenchVault::new(1, INCREMENTAL_NOTE_BYTES);
    let note = fixture.root.join("knowledge/000/note-00000.md");
    let sequence = Cell::new(0_usize);
    let mut group = criterion.benchmark_group("incremental_vault_index");
    group
        .sample_size(50)
        .warm_up_time(Duration::from_secs(1))
        .measurement_time(Duration::from_secs(3))
        .throughput(Throughput::Bytes(INCREMENTAL_NOTE_BYTES as u64));
    group.bench_function("one_50_kib_note", |bencher| {
        bencher.iter_batched(
            || {
                let next = sequence.get() + 1;
                sequence.set(next);
                fs::write(
                    &note,
                    note_body(
                        next,
                        INCREMENTAL_NOTE_BYTES,
                        Some(" incrementalrevisionmarker "),
                    ),
                )
                .expect("prepare changed benchmark note");
            },
            |()| {
                let report = reconcile_vault_paths(
                    std::hint::black_box(&fixture.storage),
                    "benchmark",
                    &fixture.root,
                    [&note],
                )
                .expect("incrementally index benchmark note");
                std::hint::black_box(report);
            },
            BatchSize::SmallInput,
        );
    });
    group.finish();
}

criterion_group!(benches, benchmark_fts_query, benchmark_incremental_note);
criterion_main!(benches);
