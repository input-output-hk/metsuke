# The Leios key roster generator (ADR 0011), apart from the serving binary for
# the same reason the allowlist is: the server reads the file this writes and
# links no chain client to do it.
{ pkgs }:
let
  source = ../tools/roster;
in
{
  # cardano-cli is not wrapped in: which era's subcommand answers is a fact
  # about the network this runs against, and so is which build of the cli
  # speaks it. The caller puts one on PATH.
  package = pkgs.writers.writeNuBin "metsuke-roster" { } (builtins.readFile "${source}/roster.nu");

  tests = pkgs.runCommand "metsuke-roster-tests" { } ''
    cp -r ${source} ./roster
    ${pkgs.nushell}/bin/nu --no-config-file ./roster/test.nu
    touch $out
  '';
}
