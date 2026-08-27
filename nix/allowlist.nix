# The offline application-code gate, apart from the serving binary so the
# server links no postgres and no csv reader (metsuke-jfb.7).
{ pkgs }:
let
  source = ../tools/allowlist;
in
{
  package = pkgs.writers.writeNuBin "metsuke-allowlist" {
    makeWrapperArgs = [
      "--prefix"
      "PATH"
      ":"
      "${pkgs.postgresql}/bin"
    ];
  } (builtins.readFile "${source}/allowlist.nu");

  tests = pkgs.runCommand "metsuke-allowlist-tests" { } ''
    ${pkgs.nushell}/bin/nu --no-config-file ${source}/test.nu
    touch $out
  '';
}
