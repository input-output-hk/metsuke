# What both metsuke units run under, written once so the NixOS modules and the
# unit shipped for hosts that are not NixOS cannot drift apart. ADR 0007 is
# what the agent's share of it has to hold.
let
  # AF_NETLINK is not reachability: glibc asks the kernel which addresses the
  # host has before it resolves a name, and both units name hosts.
  agentAddressFamilies = [
    "AF_INET"
    "AF_INET6"
    "AF_NETLINK"
  ];
in
{
  restartSecs = 30;

  inherit agentAddressFamilies;
  # The server reads db-sync over a colocated unix socket (ADR 0009). Nothing
  # in crates/metsuke/src opens one, so the agent does not get AF_UNIX.
  serverAddressFamilies = agentAddressFamilies ++ [ "AF_UNIX" ];

  hardening =
    {
      # Created and owned by systemd under /var/lib, which is the only path
      # the unit may write.
      stateDirectory,
      addressFamilies,
    }:
    {
      DynamicUser = true;
      StateDirectory = stateDirectory;

      # Emptied rather than left unset: a group grant is the thing ADR 0007
      # refuses, and an omitted setting says nothing about whether anyone
      # considered it.
      SupplementaryGroups = "";
      CapabilityBoundingSet = "";
      AmbientCapabilities = "";
      NoNewPrivileges = true;

      ProtectSystem = "strict";
      ProtectHome = true;
      ProtectProc = "invisible";
      ProcSubset = "pid";
      PrivateTmp = true;
      PrivateDevices = true;
      ProtectClock = true;
      ProtectHostname = true;
      ProtectKernelLogs = true;
      ProtectKernelModules = true;
      ProtectKernelTunables = true;
      ProtectControlGroups = true;

      RestrictAddressFamilies = addressFamilies;
      RestrictNamespaces = true;
      RestrictRealtime = true;
      RestrictSUIDSGID = true;
      LockPersonality = true;
      MemoryDenyWriteExecute = true;
      SystemCallArchitectures = "native";
      SystemCallFilter = [
        "@system-service"
        "~@privileged"
        "~@resources"
      ];

      UMask = "0077";
    };
}
