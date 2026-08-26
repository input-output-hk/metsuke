# What both metsuke units run under, written once so the NixOS modules and the
# unit shipped for hosts that are not NixOS cannot drift apart. ADR 0007 is
# what the agent's share of it has to hold, and ADR 0010 is the one exception
# it allows: a caller that says it reads the journal gets what that takes,
# everyone else renders the same bytes as before the parameter existed.
{
  restartSecs = 30;

  # AF_NETLINK is not reachability: glibc asks the kernel which addresses the
  # host has before it resolves a name, and both units name hosts. Neither
  # unit opens a unix socket, so neither gets AF_UNIX.
  addressFamilies = [
    "AF_INET"
    "AF_INET6"
    "AF_NETLINK"
  ];

  hardening =
    {
      # Created and owned by systemd under /var/lib, which is the only path
      # the unit may write.
      stateDirectory,
      addressFamilies,
      # Whether the unit runs a journalctl child (ADR 0010). One parameter
      # rather than one per directive: a unit holding the group but not what
      # journalctl needs to start would be a decision nobody made. False by
      # default, so a unit that reads no journal renders what it always did.
      readsTheJournal ? false,
    }:
    {
      DynamicUser = true;
      StateDirectory = stateDirectory;

      # A string rather than a list: an empty list renders no line at all, and
      # an omitted setting says nothing about whether anyone considered it.
      # Emptied says the grant ADR 0007 refuses was refused.
      SupplementaryGroups = if readsTheJournal then "systemd-journal" else "";
      CapabilityBoundingSet = "";
      AmbientCapabilities = "";
      NoNewPrivileges = true;

      ProtectSystem = "strict";
      ProtectHome = true;
      ProtectProc = "invisible";
      # `--follow` implies `--boot`, and the boot id is a file under /proc/sys,
      # which `pid` hides along with every other top-level path in /proc that
      # is not a task's. Without this journalctl exits before its first line.
      ProcSubset = if readsTheJournal then "all" else "pid";
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
