{ pkgs ? import
    (fetchTarball {
      # nixup: pin=jpetrucciani/nix;
      name = "jpetrucciani-2026-11-13";
      url = "https://github.com/jpetrucciani/nix/archive/990bc654233bfc3bfea5910245d6adeab377ec2c.tar.gz";
      sha256 = "1g8sjmy5jrasf0h4cbc7ikzlbfwbw8h7l8sf3lw2vaablf5pmfa8";
    })
    { overlays = [ _rust ]; }
, _rust ? import
    (fetchTarball {
      name = "oxalica-2026-11-13";
      url = "https://github.com/oxalica/rust-overlay/archive/84edd48f141b0c8910dffdd0296ddd577e829132.tar.gz";
      sha256 = "0h8j7qz8wlf27cx32g5xbgsihpd3v5psnmz2yhlsjhjyrns4hsx1";
    })
}:
let
  name = "kodo";
  target = "x86_64-unknown-linux-musl";
  # Cross builds of Windows binaries from Linux: cargo build --target x86_64-pc-windows-gnu
  windowsTarget = "x86_64-pc-windows-gnu";
  mingw = pkgs.pkgsCross.mingwW64;
  rust = (pkgs.rust-bin.fromRustupToolchainFile ./rust-toolchain.toml).override {
    extensions = [ "rust-src" "rust-analyzer" "rustfmt" "clippy" ];
    targets = [ target windowsTarget ];
  };
  rustPlatform = pkgs.makeRustPlatform {
    cargo = rust;
    rustc = rust;
  };

  # Libraries iced loads at runtime on Linux (Wayland/X11 windowing, Vulkan/GL rendering).
  guiLibs = with pkgs; [ wayland libxkbcommon vulkan-loader libGL libx11 libxcursor libxi libxrandr ];

  envVars = {
    NIXUP = "0.0.15";
    LD_LIBRARY_PATH = pkgs.lib.makeLibraryPath guiLibs;
    CARGO_TARGET_X86_64_PC_WINDOWS_GNU_LINKER = "x86_64-w64-mingw32-gcc";
    CARGO_TARGET_X86_64_PC_WINDOWS_GNU_RUSTFLAGS = "-L native=${mingw.windows.pthreads}/lib";
    # C deps (aws-lc for TLS) must use the mingw compiler, not the host gcc.
    CC_x86_64_pc_windows_gnu = "x86_64-w64-mingw32-gcc";
    AR_x86_64_pc_windows_gnu = "x86_64-w64-mingw32-ar";
    # aws-lc uses pthreads on mingw; the cross cc doesn't put winpthreads on its include path.
    CFLAGS_x86_64_pc_windows_gnu = "-I${mingw.windows.pthreads}/include";
  };
  tools = with pkgs; {
    cli = [
      actionlint
      jfmt
      nixup
    ];
    rust = [
      cargo-deny
      cargo-nextest
      cargo-zigbuild
      rust
      pkg-config
      openssl
    ];
    windows = [
      mingw.stdenv.cc
      nasm
    ];
    scripts = pkgs.lib.attrsets.attrValues scripts;
  };

  scripts = with pkgs; { };

  src = pkgs.lib.cleanSourceWith {
    src = ./.;
    filter = path: _type: !(builtins.elem (baseNameOf path) [ "target" ".direnv" "result" ]);
  };
  package = args: rustPlatform.buildRustPackage ({
    version = (builtins.fromTOML (builtins.readFile ./Cargo.toml)).workspace.package.version;
    inherit src;
    cargoLock.lockFile = ./Cargo.lock;
    # reqwest's rustls uses aws-lc-rs. Something in the Nix build env sets
    # AWS_LC_SYS_USE_SYSTEM=1 but the lookup fails; build it from source like every other target.
    preBuild = "unset AWS_LC_SYS_USE_SYSTEM";
    # The test suite runs in CI; package builds only compile.
    doCheck = false;
  } // args);

  # nix-build -A kodo-cli: headless CLI, no GUI libraries.
  kodo-cli = package {
    pname = "kodo-cli";
    cargoBuildFlags = [ "-p" "kodo-cli" ];
  };

  # nix-build -A kodo (or nix run -f . kodo): GUI + CLI, with iced's runtime libraries wrapped in.
  kodo = package {
    pname = "kodo";
    cargoBuildFlags = [ "-p" "kodo-app" "-p" "kodo-cli" ];
    nativeBuildInputs = [ pkgs.makeWrapper ];
    postInstall = ''
      install -Dm644 crates/kodo-app/assets/dev.cobi.kodo.desktop $out/share/applications/dev.cobi.kodo.desktop
      install -Dm644 crates/kodo-app/assets/icon/kodo.png $out/share/icons/hicolor/512x512/apps/dev.cobi.kodo.png
    '';
    postFixup = ''
      wrapProgram $out/bin/kodo-app --prefix LD_LIBRARY_PATH : ${pkgs.lib.makeLibraryPath guiLibs}
    '';
    meta.mainProgram = "kodo-app";
  };
  paths = pkgs.lib.flatten [ (builtins.attrValues tools) ];
  env = pkgs.buildEnv {
    inherit name paths; buildInputs = paths;
  };
in
(env.overrideAttrs (old: {
  inherit name;
  env = (old.env or { }) // envVars;
})) // { inherit scripts kodo kodo-cli; }
