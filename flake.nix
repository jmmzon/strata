{
  description = "Strata, a keyboard-first GTK4 file manager for Linux";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    # vt-1.0.2, the commit libghostty-rs binds (GHOSTTY_COMMIT in its build.rs).
    # Bump both together: the pre-1.0 C API changes between revisions.
    ghostty.url = "github:ghostty-org/ghostty/573c4a21e02311ae87844af26b6a7a179f82858e";
  };

  outputs =
    { self, nixpkgs, ghostty }:
    let
      systems = [
        "x86_64-linux"
        "aarch64-linux"
      ];
      forAllSystems = nixpkgs.lib.genAttrs systems;
    in
    {
      packages = forAllSystems (
        system:
        let
          pkgs = nixpkgs.legacyPackages.${system};
          strata = pkgs.callPackage ./package.nix {
            libghostty-vt = ghostty.packages.${system}.libghostty-vt-releasefast;
            buildCommit = self.shortRev or self.dirtyShortRev or null;
          };
        in
        {
          inherit strata;
          default = strata;
        }
      );

      checks = forAllSystems (
        system:
        let
          inherit (self.packages.${system}) strata;
        in
        {
          inherit strata;
          inherit (strata) fileChooserPortal fileManagerService;
        }
      );
    };
}
