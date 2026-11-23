{
  description = "ouija: ask a System One decision model typed questions from the shell";

  inputs.nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";

  outputs =
    { self, nixpkgs }:
    let
      systems = [
        "x86_64-linux"
        "aarch64-linux"
        "aarch64-darwin"
        "x86_64-darwin"
      ];
      forAllSystems = f: nixpkgs.lib.genAttrs systems (system: f nixpkgs.legacyPackages.${system});

      manifest = (builtins.fromTOML (builtins.readFile ./Cargo.toml)).package;

      mkOuija =
        pkgs:
        pkgs.rustPlatform.buildRustPackage {
          pname = manifest.name;
          version = manifest.version;
          src = self;
          cargoLock.lockFile = ./Cargo.lock;
          # The unit tests are pure and never contact a server, so they run
          # offline inside the sandbox.
          doCheck = true;
          meta = {
            description = manifest.description;
            homepage = manifest.repository;
            license = pkgs.lib.licenses.asl20;
            mainProgram = "ouija";
          };
        };
    in
    {
      packages = forAllSystems (pkgs: rec {
        ouija = mkOuija pkgs;
        default = ouija;
      });

      apps = forAllSystems (pkgs: {
        default = {
          type = "app";
          meta.description = manifest.description;
          program = "${self.packages.${pkgs.stdenv.hostPlatform.system}.default}/bin/ouija";
        };
      });

      devShells = forAllSystems (pkgs: {
        default = pkgs.mkShell {
          packages = with pkgs; [
            cargo
            rustc
            clippy
            rustfmt
            rust-analyzer
          ];
        };
      });

      overlays.default = final: _prev: {
        ouija = mkOuija final;
      };

      homeManagerModules = rec {
        ouija =
          {
            config,
            lib,
            pkgs,
            ...
          }:
          let
            cfg = config.programs.ouija;
            tomlFormat = pkgs.formats.toml { };
          in
          {
            options.programs.ouija = {
              enable = lib.mkEnableOption "ouija, a CLI for System One decision models";

              package = lib.mkOption {
                type = lib.types.package;
                default = self.packages.${pkgs.stdenv.hostPlatform.system}.default;
                defaultText = lib.literalExpression "ouija.packages.\${system}.default";
                description = "The ouija package to install.";
              };

              settings = lib.mkOption {
                type = lib.types.submodule {
                  freeformType = tomlFormat.type;
                  options = {
                    url = lib.mkOption {
                      type = lib.types.nullOr lib.types.str;
                      default = null;
                      example = "http://spark:18440";
                      description = "Base URL of the decision server.";
                    };
                    min_conf = lib.mkOption {
                      type = lib.types.nullOr (lib.types.either lib.types.ints.unsigned lib.types.float);
                      default = null;
                      example = 0.8;
                      description = "Confidence below this is reported as uncertain (exit 2). Must be between 0 and 1.";
                    };
                    timeout = lib.mkOption {
                      type = lib.types.nullOr (lib.types.ints.between 1 86400);
                      default = null;
                      example = 60;
                      description = "Request timeout in seconds, from 1 to 86400.";
                    };
                    flavor = lib.mkOption {
                      type = lib.types.nullOr lib.types.bool;
                      default = null;
                      example = false;
                      description = "Whether to print the flavor line on interactive terminals.";
                    };
                  };
                };
                default = { };
                description = ''
                  Contents of `$XDG_CONFIG_HOME/ouija/config.toml`. Unset (null)
                  keys are omitted so ouija's built-in defaults apply.
                '';
              };
            };

            config = lib.mkIf cfg.enable {
              home.packages = [ cfg.package ];
              xdg.configFile."ouija/config.toml" =
                let
                  set = lib.filterAttrs (_: v: v != null) cfg.settings;
                in
                lib.mkIf (set != { }) {
                  source = tomlFormat.generate "ouija-config.toml" set;
                };
            };
          };
        default = ouija;
      };
    };
}
