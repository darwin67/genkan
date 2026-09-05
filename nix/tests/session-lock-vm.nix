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
    probe_runtime = f"{runtime}/genkan-probe"
    display = f"$(basename $(find {runtime} -maxdepth 1 -type s -name 'wayland-*' | head -1))"
    ipc = f"$(find {runtime} -maxdepth 1 -type s -name 'sway-ipc.*.sock' | head -1)"
    environment = f"XDG_RUNTIME_DIR={runtime} WAYLAND_DISPLAY={display} SWAYSOCK={ipc}"
    probe_environment = f"XDG_RUNTIME_DIR={probe_runtime} WAYLAND_DISPLAY={runtime}/{display}"

    def as_alice(command):
        return f"runuser -u alice -- env {environment} {command}"

    def count(path, pattern):
        return int(machine.succeed(f"grep -Fc '{pattern}' {path} || true"))

    def client_counts():
        return (
            count("/tmp/client-events", "wl_keyboard] key:"),
            count("/tmp/client-events", "wl_pointer]"),
        )

    def stable_client_counts():
        previous = client_counts()
        for _ in range(10):
            machine.sleep(timedelta(milliseconds=200))
            current = client_counts()
            if current == previous:
                return current
            previous = current
        raise Exception("ordinary client input event counts did not stabilize")

    def archive_observer(path="/tmp/observer"):
        machine.succeed(f"test ! -f {path} || (cat {path} >>/tmp/all-observer && rm -f {path})")

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
        machine.succeed("rm -f /tmp/client-events")
        machine.execute(f"{as_alice('stdbuf -oL wev')} >/tmp/client-events 2>&1 &")
        machine.wait_until_succeeds(f"{as_alice('swaymsg -t get_tree')} | grep -F '\"app_id\": \"wev\"'")
        machine.succeed(f"{as_alice('wtype before-lock')}")
        machine.wait_until_succeeds("grep -E 'wl_keyboard] key:' /tmp/client-events")

    def stop_sway():
        machine.execute("kill $(cat /tmp/sway.pid) 2>/dev/null || true")
        machine.execute("pkill -u alice -x sway 2>/dev/null || true")
        machine.sleep(1)

    def start_lock(extra="", outputs=2, wait_for_opaque=True):
        archive_observer()
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
        if wait_for_opaque:
            machine.wait_until_succeeds(
                f"test $(grep -Fc 'committed first opaque buffer for output' /tmp/lock.log) -ge {outputs}"
            )

    def send_response(path):
        machine.succeed(as_alice(f'sh -c "cat {path} | wtype - -s 50 -k Return"'))

    def wait_for_event(event, count=1):
        machine.wait_until_succeeds(f"test $(grep -Fc {event} /tmp/observer) -ge {count}")

    def wait_for_status(path="/tmp/lock.status"):
        machine.wait_until_succeeds(f"test -f {path} && grep -Eq '^[0-9]+$' {path}")
        return int(machine.succeed(f"cat {path}"))

    def inject_isolated(label):
        client_before = stable_client_counts()
        keyboard_before = count("/tmp/observer", "KEYBOARD")
        pointer_before = count("/tmp/observer", "POINTER")
        machine.succeed(as_alice(f"wtype -s 50 {label}"))
        machine.succeed(as_alice("wlrctl pointer move 2 2"))
        machine.succeed(as_alice("wlrctl pointer click"))
        machine.wait_until_succeeds(
            f"test $(grep -Fc KEYBOARD /tmp/observer) -gt {keyboard_before}"
        )
        machine.wait_until_succeeds(
            f"test $(grep -Fc POINTER /tmp/observer) -gt {pointer_before}"
        )
        machine.sleep(timedelta(milliseconds=500))
        assert client_counts() == client_before
        return client_before

    def inject_client_blocked(label):
        client_before = stable_client_counts()
        machine.succeed(as_alice(f"wtype -s 50 {label}"))
        machine.succeed(as_alice("wlrctl pointer move 2 2"))
        machine.succeed(as_alice("wlrctl pointer click"))
        machine.sleep(timedelta(milliseconds=500))
        assert client_counts() == client_before
        return client_before

    def assert_lock_unavailable():
        # Use an independent coordination namespace and the compositor's
        # absolute socket path so this probes protocol ownership directly.
        # Sway may leave a second lock request pending instead of immediately
        # sending finished, so accept rejection or no acquisition by timeout.
        machine.succeed(
            f"rm -rf {probe_runtime}; "
            f"install -d -m 0700 -o alice -g users {probe_runtime}; "
            "rm -f /tmp/probe.ready /tmp/probe.observer /tmp/probe.status /tmp/probe.log"
        )
        machine.execute(
            "runuser -u alice -- sh -c '"
            f"env {probe_environment} timeout 5 genkan lock --reduce-motion --ready-fd 3 "
            "--test-observer-fd 4 3>/tmp/probe.ready 4>/tmp/probe.observer "
            "2>/tmp/probe.log; printf \"%s\\n\" \"$?\" >/tmp/probe.status' &"
        )
        status = wait_for_status("/tmp/probe.status")
        assert status != 0, "competing lock unexpectedly succeeded"
        machine.succeed("! grep -Fx LOCKED /tmp/probe.observer")
        machine.succeed("test ! -s /tmp/probe.ready")
        if status == 124:
            machine.succeed("grep -Fx OUTPUT_ADDED /tmp/probe.observer")
        else:
            machine.succeed("grep -Fx FINISHED /tmp/probe.observer")
        archive_observer("/tmp/probe.observer")

    def recover_lock(label, client_baseline):
        start_lock("--test-unlock-after-ready")
        assert inject_isolated(label) == client_baseline
        assert wait_for_status() == 0

    machine.start()
    machine.wait_for_unit("multi-user.target")
    machine.succeed("rm -f /tmp/all-observer")
    machine.succeed("printf factor-ok > /tmp/factor")
    machine.succeed("printf password-ok > /tmp/password")
    machine.succeed("printf password-bad > /tmp/bad-password")
    machine.succeed("chown alice:users /tmp/factor /tmp/password /tmp/bad-password")
    machine.succeed("chmod 0600 /tmp/factor /tmp/password /tmp/bad-password")

    with subtest("lock coverage, input isolation, and authentication"):
        start_sway()
        client_baseline = stable_client_counts()

        start_lock()
        assert inject_isolated("isolated") == client_baseline

        geometry_before = count("/tmp/observer", "GEOMETRY")
        machine.succeed(f"{as_alice('swaymsg output HEADLESS-2 scale 2')}")
        machine.wait_until_succeeds(
            f"test $(grep -Fc GEOMETRY /tmp/observer) -gt {geometry_before}"
        )
        output_before = count("/tmp/observer", "OUTPUT_ADDED")
        opaque_before = count("/tmp/lock.log", "committed first opaque buffer for output")
        machine.succeed(f"{as_alice('swaymsg create_output')}")
        machine.wait_until_succeeds(
            f"test $(grep -Fc OUTPUT_ADDED /tmp/observer) -gt {output_before}"
        )
        machine.wait_until_succeeds(
            f"test $(grep -Fc 'committed first opaque buffer for output' /tmp/lock.log) -gt {opaque_before}"
        )
        removed_before = count("/tmp/observer", "OUTPUT_REMOVED")
        machine.succeed(f"{as_alice('swaymsg output HEADLESS-3 disable')}")
        machine.wait_until_succeeds(
            f"test $(grep -Fc OUTPUT_REMOVED /tmp/observer) -gt {removed_before}"
        )

        wait_for_event("AUTH_PROMPT")
        send_response("/tmp/factor")
        wait_for_event("AUTH_PROMPT", 2)
        send_response("/tmp/bad-password")
        wait_for_event("AUTH_FAILURE")
        machine.succeed("kill -0 $(cat /tmp/lock.pid)")
        assert inject_isolated("failed-auth-isolated") == client_baseline
        machine.succeed(f"{as_alice('wtype -s 50 -k Return')}")
        wait_for_event("AUTH_RETRY")
        wait_for_event("AUTH_PROMPT", 3)
        send_response("/tmp/factor")
        wait_for_event("AUTH_PROMPT", 4)
        send_response("/tmp/password")
        wait_for_event("AUTH_SUCCESS")
        assert wait_for_status() == 0
        assert stable_client_counts() == client_baseline
        machine.succeed(f"{as_alice('swaymsg [app_id=wev] focus')}")
        machine.wait_until_succeeds(
            f"{as_alice('swaymsg -t get_tree')} | "
            "jq -e '.. | objects | select(.app_id? == \"wev\") | .focused == true'"
        )
        post_unlock_baseline = stable_client_counts()
        assert post_unlock_baseline == client_baseline
        machine.succeed(f"{as_alice('wtype -s 50 after-unlock')}")
        machine.succeed(f"{as_alice('wlrctl pointer move 2 2')}")
        machine.succeed(f"{as_alice('wlrctl pointer click')}")
        machine.wait_until_succeeds(
            f"test $(grep -Fc 'wl_keyboard] key:' /tmp/client-events) -gt {post_unlock_baseline[0]}"
        )
        machine.wait_until_succeeds(
            f"test $(grep -Fc 'wl_pointer]' /tmp/client-events) -gt {post_unlock_baseline[1]}"
        )

    with subtest("before-sleep waits for readiness and lock survives simulated resume"):
        archive_observer()
        machine.succeed(
            "rm -f /tmp/before-sleep /tmp/before-sleep-duplicate "
            "/tmp/before-sleep.log /tmp/before-sleep-duplicate.log /tmp/simulated-resume"
        )
        before_sleep_command = as_alice(
            'sh -c "genkan lock --daemonize --reduce-motion '
            '--test-ready-delay-ms 5000 '
            '&& touch /tmp/before-sleep"'
        )
        machine.execute(f"{before_sleep_command} >/tmp/before-sleep.log 2>&1 &")
        machine.wait_until_succeeds(
            "grep -F 'compositor confirmed lock' /tmp/before-sleep.log",
            timeout=timedelta(seconds=30),
        )
        duplicate_command = as_alice(
            'sh -c "genkan lock --daemonize --reduce-motion '
            '&& touch /tmp/before-sleep-duplicate"'
        )
        machine.execute(
            f"{duplicate_command} >/tmp/before-sleep-duplicate.log 2>&1 &"
        )
        machine.sleep(timedelta(milliseconds=500))
        machine.succeed("test ! -e /tmp/before-sleep")
        machine.succeed("test ! -e /tmp/before-sleep-duplicate")
        machine.wait_until_succeeds("test -e /tmp/before-sleep", timeout=timedelta(seconds=30))
        machine.wait_until_succeeds(
            "test -e /tmp/before-sleep-duplicate", timeout=timedelta(seconds=30)
        )
        assert_lock_unavailable()
        resume_baseline = inject_client_blocked("before-resume-isolated")
        machine.succeed("touch /tmp/simulated-resume")
        assert_lock_unavailable()
        assert inject_client_blocked("after-resume-isolated") == resume_baseline
        machine.execute("pkill -u alice -x genkan 2>/dev/null || true")
        stop_sway()
        start_sway()

    with subtest("worker death retains lock and permits explicit retry"):
        start_lock("--test-worker-failure-after-ready")
        wait_for_event("AUTH_FAILURE")
        machine.succeed("kill -0 $(cat /tmp/lock.pid)")
        inject_isolated("worker-failure-isolated")
        machine.succeed(f"{as_alice('wtype -s 50 -k Return')}")
        wait_for_event("AUTH_RETRY")
        wait_for_event("AUTH_PROMPT")
        send_response("/tmp/factor")
        wait_for_event("AUTH_PROMPT", 2)
        send_response("/tmp/password")
        wait_for_event("AUTH_SUCCESS")
        assert wait_for_status() == 0

    with subtest("panic and renderer failure never request unlock"):
        for fault in ["--test-panic-after-ready", "--test-renderer-failure-after-ready"]:
            start_lock(fault, wait_for_opaque=False)
            expected_status = 101 if fault == "--test-panic-after-ready" else 1
            assert wait_for_status() == expected_status
            fault_baseline = inject_client_blocked(f"{fault[7:]}-blocked")
            recover_lock(f"{fault[7:]}-recovery", fault_baseline)
            stop_sway()
            start_sway()

    with subtest("SIGKILL and compositor disconnect fail closed"):
        start_lock()
        machine.succeed("kill -KILL $(cat /tmp/lock.pid)")
        assert wait_for_status() == 137
        killed_baseline = inject_client_blocked("sigkill-blocked")
        recover_lock("sigkill-recovery", killed_baseline)
        stop_sway()
        start_sway()
        start_lock()
        stop_sway()
        assert wait_for_status() == 1

    with subtest("existing compositor lock is denied"):
        start_sway()
        machine.execute(f"{as_alice('swaylock -f -c 000000')} >/tmp/swaylock.log 2>&1 &")
        machine.sleep(timedelta(seconds=1))
        assert_lock_unavailable()

    archive_observer()
    machine.succeed(
        "grep -Ev '^(LOCKED|FAILED|FINISHED|OUTPUT_ADDED|OUTPUT_REMOVED|GEOMETRY|KEYBOARD|POINTER|"
        "AUTH_PROMPT|AUTH_RETRY|AUTH_SUCCESS|AUTH_FAILURE)$' "
        "/tmp/all-observer && exit 1 || true"
    )
    for response in ["factor-ok", "password-ok", "password-bad"]:
        machine.succeed(f"! grep -F {response} /tmp/all-observer")
  '';
}
