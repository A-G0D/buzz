// Evaluated by the macOS native-window reveal plugin when React has not
// committed its first surface. Keep this dependency-free so it can still run
// when the frontend bundle or bootstrap has failed.
(() => {
  const id = "buzz-native-startup-recovery";
  const existing = document.getElementById(id);
  const action = window.__buzzStartupRecoveryAction;
  delete window.__buzzStartupRecoveryAction;

  if (action === "remove") {
    existing?.remove();
    return;
  }

  if (existing || !document.body) return;
  const overlay = document.createElement("main");
  overlay.id = id;
  overlay.setAttribute("role", "alert");
  overlay.style.cssText =
    "position:fixed;inset:0;z-index:2147483647;display:grid;place-items:center;padding:24px;background:#111518;color:#f4f5f6;font:16px/1.5 -apple-system,BlinkMacSystemFont,'Segoe UI',sans-serif";

  const card = document.createElement("section");
  card.style.cssText =
    "width:min(100%,440px);padding:28px;border:1px solid #41484c;border-radius:16px;background:#1b2125;box-shadow:0 20px 60px #0006";

  const brand = document.createElement("p");
  brand.textContent = "BUZZ";
  brand.style.cssText =
    "margin:0 0 20px;color:#e5e827;font-size:13px;font-weight:700;letter-spacing:.16em";

  const title = document.createElement("h1");
  title.textContent = "Buzz is taking longer than expected to open";
  title.style.cssText =
    "margin:0;font-size:20px;line-height:1.3;font-weight:650";

  const detail = document.createElement("p");
  detail.textContent =
    "Try reloading Buzz. Your profile and local data have not been cleared.";
  detail.style.cssText = "margin:12px 0 22px;color:#c4c9cc";

  const retry = document.createElement("button");
  retry.type = "button";
  retry.textContent = "Reload Buzz";
  retry.style.cssText =
    "min-height:42px;padding:0 18px;border:0;border-radius:999px;background:#e5e827;color:#171a1c;font:inherit;font-weight:650;cursor:pointer";
  retry.addEventListener("click", () => window.location.reload());

  card.append(brand, title, detail, retry);
  overlay.append(card);
  document.body.append(overlay);
})();
