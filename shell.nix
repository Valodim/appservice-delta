with import <nixpkgs> {};
let
  src = fetchFromGitHub {
      owner = "mozilla";
      repo = "nixpkgs-mozilla";
      # commit from: 2019-07-15
      rev = "8c007b60731c07dd7a052cce508de3bb1ae849b4";
      sha256 = "sha256-RsNPnEKd7BcogwkqhaV5kI/HuNC4flH/OQCC/4W5y/8=";
   };
  rustOverlay = import "${src.out}/rust-overlay.nix" pkgs pkgs;
  rustChannel = (rustOverlay.rustChannelOf { rustToolchain = ./rust-toolchain; });
in
stdenv.mkDerivation {
  name = "rust-env";
  buildInputs = [
    rustChannel.rust
    # latest.rustChannels.nightly.rust
    # latest.rustChannels.stable.rust

    matrix-synapse

    clang
    nettle
    pkgconfig

    gettext
    transifex-client
  ];

  # Set Environment Variables
  RUST_BACKTRACE = 1;

  # compilation of -sys packages requires manually setting this :(
  shellHook = ''
    export LIBCLANG_PATH="${pkgs.llvmPackages.libclang}/lib";
  '';
}
