{ genkanE2e }:
{
  name = "genkan-greetd-e2e";

  nodes.machine =
    { lib, pkgs, ... }:
    let
      session = pkgs.writeShellScript "genkan-e2e-session" ''
        set -eu
        ${pkgs.coreutils}/bin/env > /tmp/genkan-e2e.env
        ${pkgs.coreutils}/bin/id -un > /tmp/genkan-e2e.user
        ${pkgs.coreutils}/bin/touch /tmp/genkan-e2e.passed
        exec ${pkgs.coreutils}/bin/sleep infinity
      '';
    in
    {
      users.mutableUsers = false;
      users.users.alice = {
        isNormalUser = true;
        password = "correct-password";
      };

      services.greetd = {
        enable = true;
        settings.default_session.command = lib.concatStringsSep " " [
          "${genkanE2e}/bin/genkan-greetd-e2e"
          "--username alice"
          "--wrong-password wrong-password"
          "--password correct-password"
          "--session-command ${session}"
        ];
      };
    };

  testScript = ''
    from datetime import timedelta

    machine.start()
    machine.wait_for_unit("greetd.service")
    machine.wait_until_succeeds(
        "test -f /tmp/genkan-e2e.passed",
        timeout=timedelta(seconds=60),
    )
    machine.succeed("grep -Fx 'GENKAN_E2E=passed' /tmp/genkan-e2e.env")
    machine.succeed("grep -Fx 'alice' /tmp/genkan-e2e.user")
  '';
}
