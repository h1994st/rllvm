use std::fs;
use std::path::{Path, PathBuf};

use criterion::{Criterion, black_box, criterion_group, criterion_main};
use object::{BinaryFormat, SectionKind, write};
use rllvm::arg_parser::CompilerArgsInfo;
use rllvm::cache::compute_cache_key;
use rllvm::utils::{
    calculate_filepath_hash, embed_bitcode_filepath_to_object_file,
    extract_bitcode_filepaths_from_object_file, extract_bitcode_filepaths_from_parsed_object,
};

/// Path to test data bundled with the project.
fn test_data(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/data")
        .join(name)
}

/// Create a minimal Mach-O object file for benchmarking on macOS.
fn create_minimal_macho_object(path: &Path) {
    let mut obj = write::Object::new(
        BinaryFormat::MachO,
        object::Architecture::Aarch64,
        object::Endianness::Little,
    );
    let section_id = obj.add_section(
        b"__TEXT".to_vec(),
        b"__text".to_vec(),
        SectionKind::Text,
    );
    let section = obj.section_mut(section_id);
    // ARM64 `ret` instruction
    section.set_data(&[0xc0, 0x03, 0x5f, 0xd6], 4);
    let data = obj.write().expect("failed to write Mach-O object");
    fs::write(path, data).expect("failed to write object file");
}

/// Create a minimal COFF object file for benchmarking.
fn create_minimal_coff_object(path: &Path) {
    let mut obj = write::Object::new(
        BinaryFormat::Coff,
        object::Architecture::X86_64,
        object::Endianness::Little,
    );
    let section_id = obj.add_section(vec![], b".text".to_vec(), SectionKind::Text);
    let section = obj.section_mut(section_id);
    section.set_data(&[0xc3], 1);
    let data = obj.write().expect("failed to write COFF object");
    fs::write(path, data).expect("failed to write COFF file");
}

// ---------------------------------------------------------------------------
// Bitcode embedding benchmarks
// ---------------------------------------------------------------------------

fn bench_bitcode_embedding(c: &mut Criterion) {
    let mut group = c.benchmark_group("bitcode_embedding");

    let dir = tempfile::tempdir().expect("failed to create temp dir");

    // Use the real hello.o test data if available, otherwise create a synthetic one
    let hello_o = test_data("hello.o");
    let source_obj = if hello_o.exists() {
        hello_o
    } else {
        let p = dir.path().join("synthetic.o");
        create_minimal_macho_object(&p);
        p
    };

    group.bench_function("embed_macho_or_native", |b| {
        let output = dir.path().join("bench_embed_output.o");
        let bc_path = Path::new("/tmp/bench_test.bc");
        b.iter(|| {
            embed_bitcode_filepath_to_object_file(
                black_box(bc_path),
                black_box(source_obj.as_path()),
                Some(black_box(output.as_path())),
            )
            .expect("embed failed");
        });
    });

    // Also benchmark with a COFF object
    let coff_obj = dir.path().join("bench_coff.obj");
    create_minimal_coff_object(&coff_obj);

    group.bench_function("embed_coff", |b| {
        let output = dir.path().join("bench_coff_output.obj");
        let bc_path = Path::new("/tmp/bench_test.bc");
        b.iter(|| {
            embed_bitcode_filepath_to_object_file(
                black_box(bc_path),
                black_box(coff_obj.as_path()),
                Some(black_box(output.as_path())),
            )
            .expect("embed failed");
        });
    });

    group.finish();
}

// ---------------------------------------------------------------------------
// Bitcode extraction benchmarks
// ---------------------------------------------------------------------------

fn bench_bitcode_extraction(c: &mut Criterion) {
    let mut group = c.benchmark_group("bitcode_extraction");

    let dir = tempfile::tempdir().expect("failed to create temp dir");

    // Create an object with embedded bitcode for extraction benchmarks
    let hello_o = test_data("hello.o");
    let embedded_obj = dir.path().join("embedded.o");

    let source_obj = if hello_o.exists() {
        hello_o
    } else {
        let p = dir.path().join("source.o");
        create_minimal_macho_object(&p);
        p
    };

    embed_bitcode_filepath_to_object_file(
        Path::new("/tmp/bench.bc"),
        source_obj.as_path(),
        Some(embedded_obj.as_path()),
    )
    .expect("setup: embed failed");

    group.bench_function("extract_from_file", |b| {
        b.iter(|| {
            extract_bitcode_filepaths_from_object_file(black_box(embedded_obj.as_path()))
                .expect("extract failed");
        });
    });

    group.bench_function("extract_from_parsed_object", |b| {
        let data = fs::read(&embedded_obj).expect("read failed");
        let parsed = object::File::parse(&*data).expect("parse failed");
        b.iter(|| {
            extract_bitcode_filepaths_from_parsed_object(black_box(&parsed))
                .expect("extract failed");
        });
    });

    // Benchmark extraction from the bundled dylib (multiple bitcode paths)
    let dylib_path = test_data("foo_bar_baz.dylib");
    if dylib_path.exists() {
        group.bench_function("extract_from_dylib_3_paths", |b| {
            b.iter(|| {
                extract_bitcode_filepaths_from_object_file(black_box(dylib_path.as_path()))
                    .expect("extract failed");
            });
        });
    }

    group.finish();
}

// ---------------------------------------------------------------------------
// File hashing / cache key benchmarks
// ---------------------------------------------------------------------------

fn bench_cache_key(c: &mut Criterion) {
    let mut group = c.benchmark_group("cache_key");

    let dir = tempfile::tempdir().expect("failed to create temp dir");

    // Small source file
    let small_src = dir.path().join("small.c");
    fs::write(&small_src, "int main() { return 0; }").unwrap();

    // Medium source file (~10 KB)
    let medium_src = dir.path().join("medium.c");
    let medium_content: String = (0..200)
        .map(|i| format!("int func_{i}(int x) {{ return x + {i}; }}\n"))
        .collect();
    fs::write(&medium_src, &medium_content).unwrap();

    // Large source file (~100 KB)
    let large_src = dir.path().join("large.c");
    let large_content: String = (0..2000)
        .map(|i| format!("int func_{i}(int x) {{ return x + {i}; }}\n"))
        .collect();
    fs::write(&large_src, &large_content).unwrap();

    let args: Vec<String> = vec![
        "-O2", "-Wall", "-Wextra", "-std=c11", "-fPIC", "-DNDEBUG",
    ]
    .into_iter()
    .map(String::from)
    .collect();

    group.bench_function("small_file", |b| {
        b.iter(|| {
            compute_cache_key(black_box(&small_src), black_box(&args), None)
                .expect("cache key failed");
        });
    });

    group.bench_function("medium_file_10kb", |b| {
        b.iter(|| {
            compute_cache_key(black_box(&medium_src), black_box(&args), None)
                .expect("cache key failed");
        });
    });

    group.bench_function("large_file_100kb", |b| {
        b.iter(|| {
            compute_cache_key(black_box(&large_src), black_box(&args), None)
                .expect("cache key failed");
        });
    });

    let extra_flags = vec!["-emit-llvm".to_string(), "-flto".to_string()];
    group.bench_function("with_bitcode_flags", |b| {
        b.iter(|| {
            compute_cache_key(
                black_box(&small_src),
                black_box(&args),
                Some(black_box(&extra_flags)),
            )
            .expect("cache key failed");
        });
    });

    group.finish();
}

// ---------------------------------------------------------------------------
// Object file parsing benchmarks
// ---------------------------------------------------------------------------

fn bench_object_file_parsing(c: &mut Criterion) {
    let mut group = c.benchmark_group("object_file_parsing");

    // Parse the bundled hello.o
    let hello_o = test_data("hello.o");
    if hello_o.exists() {
        let data = fs::read(&hello_o).expect("read hello.o");
        group.bench_function("parse_hello_o", |b| {
            b.iter(|| {
                object::File::parse(black_box(&*data)).expect("parse failed");
            });
        });
    }

    // Parse the bundled dylib
    let dylib = test_data("foo_bar_baz.dylib");
    if dylib.exists() {
        let data = fs::read(&dylib).expect("read dylib");
        group.bench_function("parse_dylib", |b| {
            b.iter(|| {
                object::File::parse(black_box(&*data)).expect("parse failed");
            });
        });
    }

    // Parse a synthetic COFF object
    let dir = tempfile::tempdir().expect("failed to create temp dir");
    let coff_path = dir.path().join("bench.obj");
    create_minimal_coff_object(&coff_path);
    let coff_data = fs::read(&coff_path).expect("read coff");
    group.bench_function("parse_coff_minimal", |b| {
        b.iter(|| {
            object::File::parse(black_box(&*coff_data)).expect("parse failed");
        });
    });

    group.finish();
}

// ---------------------------------------------------------------------------
// Path hashing benchmarks
// ---------------------------------------------------------------------------

fn bench_filepath_hash(c: &mut Criterion) {
    let mut group = c.benchmark_group("filepath_hash");

    group.bench_function("short_path", |b| {
        b.iter(|| {
            calculate_filepath_hash(black_box(Path::new("/tmp/foo.c")));
        });
    });

    group.bench_function("long_path", |b| {
        let long_path = PathBuf::from("/home/user/projects/very/deeply/nested/directory/structure/with/many/components/source_file.c");
        b.iter(|| {
            calculate_filepath_hash(black_box(&long_path));
        });
    });

    group.finish();
}

// ---------------------------------------------------------------------------
// Argument parsing benchmarks
// ---------------------------------------------------------------------------

fn bench_arg_parsing(c: &mut Criterion) {
    let mut group = c.benchmark_group("arg_parsing");

    // Simple compile command
    let simple_args = vec!["-c", "test.c", "-o", "test.o"];
    group.bench_function("simple_compile", |b| {
        b.iter(|| {
            let mut info = CompilerArgsInfo::default();
            info.parse_args(black_box(&simple_args)).expect("parse failed");
        });
    });

    // Typical compile command with many flags
    let typical_args = vec![
        "-O2",
        "-Wall",
        "-Wextra",
        "-std=c11",
        "-fPIC",
        "-DNDEBUG",
        "-DVERSION=1",
        "-I/usr/include",
        "-I/usr/local/include",
        "-c",
        "src/main.c",
        "-o",
        "build/main.o",
    ];
    group.bench_function("typical_compile", |b| {
        b.iter(|| {
            let mut info = CompilerArgsInfo::default();
            info.parse_args(black_box(&typical_args))
                .expect("parse failed");
        });
    });

    // Complex link command
    let complex_args = vec![
        "-O2",
        "-Wall",
        "-fPIC",
        "-shared",
        "-Wl,-soname,libfoo.so.1",
        "foo.o",
        "bar.o",
        "baz.o",
        "-L/usr/lib",
        "-L/usr/local/lib",
        "-lm",
        "-lpthread",
        "-ldl",
        "-o",
        "libfoo.so",
    ];
    group.bench_function("complex_link", |b| {
        b.iter(|| {
            let mut info = CompilerArgsInfo::default();
            info.parse_args(black_box(&complex_args))
                .expect("parse failed");
        });
    });

    // Many flags (stress test)
    let many_flags: Vec<String> = (0..100)
        .flat_map(|i| vec![format!("-I/path/to/include/{i}"), format!("-DFLAG_{i}=1")])
        .chain(vec!["-c".to_string(), "test.c".to_string(), "-o".to_string(), "test.o".to_string()])
        .collect();
    let many_flags_ref: Vec<&str> = many_flags.iter().map(|s| s.as_str()).collect();
    group.bench_function("many_flags_200_args", |b| {
        b.iter(|| {
            let mut info = CompilerArgsInfo::default();
            info.parse_args(black_box(&many_flags_ref))
                .expect("parse failed");
        });
    });

    group.finish();
}

criterion_group!(
    benches,
    bench_bitcode_embedding,
    bench_bitcode_extraction,
    bench_cache_key,
    bench_object_file_parsing,
    bench_filepath_hash,
    bench_arg_parsing,
);
criterion_main!(benches);
