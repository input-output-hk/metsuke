# What every metsuke unit runs under, written once so the NixOS modules and the
# unit shipped for hosts that are not NixOS cannot drift apart. ADR 0007 is
# what the agent's share of it has to hold, and ADR 0010 is the one exception
# it allows: a caller that says it reads the journal gets what that takes,
# everyone else renders the same bytes as before the parameter existed.
{
  restartSecs = 30;

  # The pipe drop-in's own pacing, which is the node's rather than the agent's:
  # the drop-in supervises the node's unit. The three together are what makes
  # the outcome the drop-in's instead of whichever unit it lands on. Left unset,
  # a node with systemd's 100ms default reaches the default burst and stays in
  # `failed`, and one pacing itself in seconds retries for ever.
  #
  # These are contrib/cardano-node.service's own values, so the drop-in changes
  # nothing when it lands there, and
  # `the_pipe_dropin_paces_restarts_as_the_node_unit_does` holds the two files
  # to agreeing. That unit says why the burst is worth reaching: it is the state
  # a monitor can see. Only a pipeline that fails at once gets there, which is
  # the failure retrying cannot fix; five slow failures span far more than the
  # interval, so those keep retrying.
  nodeRestartSecs = 5;
  nodeStartLimitIntervalSecs = 60;
  nodeStartLimitBurst = 5;

  # What an operator replaces in the shipped pipe drop-in with their node's own
  # command. Unmistakable on purpose: a plausible-looking command is one
  # somebody installs as it stands, and this one cannot start a node.
  nodeCommandPlaceholder = "YOUR-NODE-COMMAND";

  # AF_NETLINK is not reachability: glibc asks the kernel which addresses the
  # host has before it resolves a name, and the units that speak HTTP name
  # hosts. No AF_UNIX: a unit that talks to a local socket asks for it, which
  # only the roster generator does (ADR 0011).
  addressFamilies = [
    "AF_INET"
    "AF_INET6"
    "AF_NETLINK"
  ];

  # What the roster generator reaches the node by: one local socket, no network
  # of its own. AF_NETLINK for the same reason as above, since cardano-cli is a
  # glibc program like the rest.
  socketOnly = [
    "AF_UNIX"
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
      # The user a unit runs as, transient unless a caller names one. A unit
      # whose output another unit reads has to be named: a DynamicUser's
      # StateDirectory sits under /var/lib/private, which is 0700 root, so no
      # second unit can traverse it however the file itself is moded.
      user ? null,
      # Groups beyond what `readsTheJournal` grants, for reaching a socket or a
      # file another unit owns.
      groups ? [ ],
      # 0077 unless a unit's output is meant to be read by its group, which is
      # the only reason to widen it.
      umask ? "0077",
    }:
    {
      DynamicUser = user == null;
      StateDirectory = stateDirectory;

      # A string rather than a list: an empty list renders no line at all, and
      # an omitted setting says nothing about whether anyone considered it.
      # Emptied says the grant ADR 0007 refuses was refused.
      SupplementaryGroups = builtins.concatStringsSep " " (
        groups ++ (if readsTheJournal then [ "systemd-journal" ] else [ ])
      );
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

      UMask = umask;
    }
    // (
      if user == null then
        { }
      else
        {
          User = user;
          Group = user;
        }
    );
}
