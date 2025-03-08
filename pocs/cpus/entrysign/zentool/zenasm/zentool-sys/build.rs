fn main() {
    let openssl = pkg_config::probe_library("openssl").unwrap();
    let gmp = pkg_config::probe_library("gmp").unwrap();
    let json_c = pkg_config::probe_library("json-c").unwrap();

    let mut build = cc::Build::new();

    build
        .files(
            [
                "disas.c",
                "options.c",
                "ucode.c",
                "parse.c",
                "ucode.c",
                "risc86.c",
                "preimage.c",
                "options.c",
                "util.c",
                "cpuid.c",
                "factor.c",
                "dump.c",
                "data.c",
                "factor.c",
            ]
            .iter()
            .map(|f| format!("../../{f}")),
        )
        .std("gnu2x")
        .flag("-mavx")
        .flag("-D_GNU_SOURCE")
        .flag("-march=znver2");

    for lib in [openssl, gmp, json_c] {
        for path in lib.include_paths {
            build.include(path);
        }
    }

    build.compile("zentool")
}
