export function renderStartupFailure(
  document: Document,
  onReload: () => void,
): boolean {
  const root = document.getElementById("root");
  if (!root) return false;

  const screen = document.createElement("main");
  screen.className =
    "flex h-screen w-screen flex-col items-center justify-center gap-3 bg-background px-6 text-foreground";
  screen.setAttribute("role", "alert");

  const title = document.createElement("p");
  title.className = "text-base font-semibold";
  title.textContent = "Buzz couldn't finish starting";

  const description = document.createElement("p");
  description.className = "max-w-md text-center text-sm text-muted-foreground";
  description.textContent =
    "Reload Buzz to try again. If this keeps happening, check that Buzz can access website data.";

  const retry = document.createElement("button");
  retry.className =
    "rounded-md border border-border bg-secondary px-4 py-2 text-sm hover:bg-secondary/80";
  retry.textContent = "Reload Buzz";
  retry.type = "button";
  retry.addEventListener("click", onReload);

  screen.append(title, description, retry);
  root.replaceChildren(screen);
  return true;
}
