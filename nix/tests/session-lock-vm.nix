{ genkan }:
{
  name = "genkan-session-lock-vm";

  nodes.machine =
    { lib, pkgs, ... }:
    let
      testPam = pkgs.stdenv.mkDerivation {
        pname = "pam-genkan-test";
        version = "1";
        dontUnpack = true;
        nativeBuildInputs = [ pkgs.pkg-config ];
        buildInputs = [ pkgs.pam ];
        buildPhase = ''
          cat > module.c <<'EOF'
          #include <security/pam_appl.h>
          #include <security/pam_modules.h>
          #include <stdlib.h>
          #include <string.h>

          PAM_EXTERN int pam_sm_authenticate(pam_handle_t *pamh, int flags,
                                              int argc, const char **argv) {
            (void)flags; (void)argc; (void)argv;
            const struct pam_conv *conversation = NULL;
            if (pam_get_item(pamh, PAM_CONV, (const void **)&conversation) != PAM_SUCCESS ||
                conversation == NULL || conversation->conv == NULL) return PAM_SYSTEM_ERR;
            const struct pam_message messages[] = {
              { PAM_TEXT_INFO, "Test authentication" },
              { PAM_PROMPT_ECHO_ON, "Factor" },
              { PAM_ERROR_MSG, "Test policy" },
              { PAM_PROMPT_ECHO_OFF, "Password" },
            };
            const struct pam_message *message_ptrs[] = {
              &messages[0], &messages[1], &messages[2], &messages[3]
            };
            struct pam_response *responses = NULL;
            int result = conversation->conv(4, message_ptrs, &responses,
                                            conversation->appdata_ptr);
            int accepted = result == PAM_SUCCESS && responses != NULL &&
              responses[1].resp != NULL && responses[3].resp != NULL &&
              strcmp(responses[1].resp, "factor-ok") == 0 &&
              strcmp(responses[3].resp, "password-ok") == 0;
            if (responses != NULL) {
              for (int index = 0; index < 4; index++) {
                if (responses[index].resp != NULL) {
                  memset(responses[index].resp, 0, strlen(responses[index].resp));
                  free(responses[index].resp);
                }
              }
              free(responses);
            }
            return accepted ? PAM_SUCCESS : PAM_AUTH_ERR;
          }
          PAM_EXTERN int pam_sm_setcred(pam_handle_t *pamh, int flags,
                                        int argc, const char **argv) {
            (void)pamh; (void)flags; (void)argc; (void)argv;
            return PAM_SUCCESS;
          }
          EOF
          $CC -shared -fPIC -Wall -Wextra -Werror module.c -o pam_genkan_test.so -lpam
        '';
        installPhase = ''
          install -Dm0555 pam_genkan_test.so $out/lib/security/pam_genkan_test.so
        '';
      };
    in
    {
      users.mutableUsers = false;
      users.users.alice = {
        isNormalUser = true;
        uid = 1000;
      };
      security.pam.services.genkan-lock.text = lib.mkForce ''
        auth required ${testPam}/lib/security/pam_genkan_test.so
      '';
      environment.systemPackages = [
        genkan
        pkgs.gnugrep
        pkgs.jq
        pkgs.sway
        pkgs.swaylock
        pkgs.wev
        pkgs.wlrctl
        pkgs.wtype
      ];
      virtualisation.memorySize = 4096;
      virtualisation.cores = 4;
      system.stateVersion = "26.05";
    };

  testScript = ''
    from datetime import timedelta

    runtime = "/run/user/1000"
    display = f"$(basename $(find {runtime} -maxdepth 1 -type s -name 'wayland-*' | head -1))"
    ipc = f"$(find {runtime} -maxdepth 1 -type s -name 'sway-ipc.*.sock' | head -1)"
    environment = f"XDG_RUNTIME_DIR={runtime} WAYLAND_DISPLAY={display} SWAYSOCK={ipc}"

    def as_alice(command):
        return f"runuser -u alice -- env {environment} {command}"

    def start_sway(outputs=2):
        machine.succeed(f"rm -rf {runtime}; install -d -m 0700 -o alice -g users {runtime}")
        machine.succeed("printf 'output * mode 800x600\\nseat * hide_cursor 1000\\n' > /tmp/sway.conf")
        machine.execute(
            "runuser -u alice -- env "
            f"XDG_RUNTIME_DIR={runtime} WLR_BACKENDS=headless WLR_HEADLESS_OUTPUTS={outputs} "
            "WLR_LIBINPUT_NO_DEVICES=1 "
            "sway -c /tmp/sway.conf -d >/tmp/sway.log 2>&1 & "
            "echo $! >/tmp/sway.pid"
        )
        machine.wait_until_succeeds(f"find {runtime} -maxdepth 1 -type s -name 'wayland-*' | grep -q .")
        machine.wait_until_succeeds(f"find {runtime} -maxdepth 1 -type s -name 'sway-ipc.*.sock' | grep -q .")
        machine.wait_until_succeeds(f"{as_alice('swaymsg -t get_outputs')} | jq -e 'length == {outputs}'")

    def stop_sway():
        machine.execute("kill $(cat /tmp/sway.pid) 2>/dev/null || true")
        machine.execute("pkill -u alice -x sway 2>/dev/null || true")
        machine.sleep(1)

    def start_lock(extra=""):
        machine.succeed("rm -f /tmp/ready /tmp/observer /tmp/lock.log /tmp/lock.status")
        machine.execute(
            "runuser -u alice -- sh -c '"
            f"env {environment} genkan lock --reduce-motion --ready-fd 3 "
            f"--test-observer-fd 4 {extra} 3>/tmp/ready 4>/tmp/observer 2>/tmp/lock.log & "
            "pid=$!; echo $pid >/tmp/lock.pid; wait $pid; echo $? >/tmp/lock.status' "
            ">/tmp/lock-wrapper.log 2>&1 &"
        )
        machine.wait_until_succeeds("grep -Fx READY /tmp/ready", timeout=timedelta(seconds=30))
        machine.succeed("grep -Fx LOCKED /tmp/observer")

    def send_response(path):
        machine.succeed(as_alice(f'sh -c "cat {path} | wtype - -s 50 -k Return"'))

    def wait_for_event(event, count=1):
        machine.wait_until_succeeds(f"test $(grep -Fc {event} /tmp/observer) -ge {count}")

    machine.start()
    machine.wait_for_unit("multi-user.target")
    machine.succeed("printf factor-ok > /tmp/factor")
    machine.succeed("printf password-ok > /tmp/password")
    machine.succeed("printf password-bad > /tmp/bad-password")
    machine.succeed("chown alice:users /tmp/factor /tmp/password /tmp/bad-password")
    machine.succeed("chmod 0600 /tmp/factor /tmp/password /tmp/bad-password")

    with subtest("lock coverage, input isolation, authentication, and resume boundary"):
        start_sway()
        machine.execute(f"{as_alice('stdbuf -oL wev')} >/tmp/client-events 2>&1 &")
        machine.wait_until_succeeds(f"{as_alice('swaymsg -t get_tree')} | grep -F '\"app_id\": \"wev\"'")
        machine.succeed(f"{as_alice('wtype before-lock')}")
        machine.wait_until_succeeds("grep -E 'wl_keyboard] key:' /tmp/client-events")
        client_keys = int(machine.succeed("grep -Ec 'wl_keyboard] key:' /tmp/client-events"))
        client_pointer_events = int(
            machine.succeed("grep -Ec 'wl_pointer]' /tmp/client-events || true")
        )

        start_lock()
        machine.succeed(f"{as_alice('wtype isolated')}")
        machine.succeed(f"{as_alice('wlrctl pointer move 2 2')}")
        machine.succeed(f"{as_alice('wlrctl pointer click')}")
        machine.wait_until_succeeds("grep -Fx KEYBOARD /tmp/observer && grep -Fx POINTER /tmp/observer")
        assert int(machine.succeed("grep -Ec 'wl_keyboard] key:' /tmp/client-events")) == client_keys
        assert int(machine.succeed("grep -Ec 'wl_pointer]' /tmp/client-events || true")) == client_pointer_events

        machine.succeed(f"{as_alice('swaymsg output HEADLESS-2 scale 2')}")
        machine.wait_until_succeeds("grep -Fx GEOMETRY /tmp/observer")
        machine.succeed(f"{as_alice('swaymsg create_output')}")
        machine.wait_until_succeeds("test $(grep -Fc OUTPUT_ADDED /tmp/observer) -ge 3")
        machine.succeed(f"{as_alice('swaymsg output HEADLESS-3 disable')}")
        machine.wait_until_succeeds("grep -Fx OUTPUT_REMOVED /tmp/observer")

        # A simulated before-sleep action reaches its marker only after the
        # duplicate launcher has joined the compositor-confirmed owner.
        machine.succeed("rm -f /tmp/before-sleep")
        machine.succeed(as_alice('sh -c "genkan lock --daemonize --reduce-motion && touch /tmp/before-sleep"'))
        machine.succeed("test -e /tmp/before-sleep")

        wait_for_event("AUTH_PROMPT")
        send_response("/tmp/factor")
        wait_for_event("AUTH_PROMPT", 2)
        send_response("/tmp/bad-password")
        wait_for_event("AUTH_FAILURE")
        machine.succeed("kill -0 $(cat /tmp/lock.pid)")
        assert int(machine.succeed("grep -Ec 'wl_keyboard] key:' /tmp/client-events")) == client_keys
        machine.succeed(f"{as_alice('wtype -s 50 -k Return')}")
        wait_for_event("AUTH_RETRY")
        wait_for_event("AUTH_PROMPT", 3)
        send_response("/tmp/factor")
        wait_for_event("AUTH_PROMPT", 4)
        send_response("/tmp/password")
        wait_for_event("AUTH_SUCCESS")
        machine.wait_until_succeeds("test -e /tmp/lock.status && grep -Fx 0 /tmp/lock.status")
        machine.succeed(f"{as_alice('swaymsg [app_id=wev] focus')}")
        machine.wait_until_succeeds(
            f"{as_alice('swaymsg -t get_tree')} | "
            "jq -e '.. | objects | select(.app_id? == \"wev\") | .focused == true'"
        )
        machine.succeed(f"{as_alice('wtype -s 50 after-unlock')}")
        machine.succeed(f"{as_alice('wlrctl pointer move 2 2')}")
        machine.succeed(f"{as_alice('wlrctl pointer click')}")
        machine.wait_until_succeeds(f"test $(grep -Ec 'wl_keyboard] key:' /tmp/client-events) -gt {client_keys}")
        machine.wait_until_succeeds(
            f"test $(grep -Ec 'wl_pointer]' /tmp/client-events) -gt {client_pointer_events}"
        )

    with subtest("worker death retains lock and permits explicit retry"):
        start_lock("--test-worker-failure-after-ready")
        wait_for_event("AUTH_FAILURE")
        machine.succeed("kill -0 $(cat /tmp/lock.pid)")
        machine.succeed(f"{as_alice('wtype -s 50 -k Return')}")
        wait_for_event("AUTH_RETRY")
        wait_for_event("AUTH_PROMPT")
        send_response("/tmp/factor")
        wait_for_event("AUTH_PROMPT", 2)
        send_response("/tmp/password")
        wait_for_event("AUTH_SUCCESS")
        machine.wait_until_succeeds("test -e /tmp/lock.status && grep -Fx 0 /tmp/lock.status")

    with subtest("panic and renderer failure never request unlock"):
        for fault in ["--test-panic-after-ready", "--test-renderer-failure-after-ready"]:
            start_lock(fault)
            machine.wait_until_succeeds("test -e /tmp/lock.status && ! grep -Fx 0 /tmp/lock.status")
            machine.fail(f"{as_alice('timeout 3 genkan lock --reduce-motion')}")
            stop_sway()
            start_sway()

    with subtest("SIGKILL and compositor disconnect fail closed"):
        start_lock()
        machine.succeed("kill -KILL $(cat /tmp/lock.pid)")
        machine.sleep(timedelta(seconds=1))
        machine.fail(f"{as_alice('timeout 3 genkan lock --reduce-motion')}")
        stop_sway()
        start_sway()
        start_lock()
        stop_sway()
        machine.wait_until_succeeds("test -e /tmp/lock.status && ! grep -Fx 0 /tmp/lock.status")

    with subtest("existing compositor lock is denied"):
        start_sway()
        machine.execute(f"{as_alice('swaylock -f -c 000000')} >/tmp/swaylock.log 2>&1 &")
        machine.sleep(timedelta(seconds=1))
        machine.succeed("rm -f /tmp/observer")
        machine.fail(
            f"{as_alice('timeout 5 genkan lock --reduce-motion --test-observer-fd 4')} "
            "4>/tmp/observer"
        )
        machine.succeed("grep -Fx FINISHED /tmp/observer")

    machine.succeed(
        "grep -Ev '^(LOCKED|FAILED|FINISHED|OUTPUT_ADDED|OUTPUT_REMOVED|GEOMETRY|KEYBOARD|POINTER|"
        "AUTH_PROMPT|AUTH_RETRY|AUTH_SUCCESS|AUTH_FAILURE)$' "
        "/tmp/observer && exit 1 || true"
    )
  '';
}
