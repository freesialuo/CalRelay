// Test-only entry. It is never an input of the production Vite build.
import { AdminSession } from "../../src/admin-session";
import { SourcesAPI } from "../../src/sources-api";
import { mountSources } from "../../src/sources-page";
import "../../src/style.css";

const session = new AdminSession();
await session.establish(
  `synthetic_browser_test_challenge_${crypto.randomUUID()}`,
  new AbortController().signal,
);
const root = document.querySelector<HTMLElement>("#fixture");
if (root) mountSources(root, new SourcesAPI(session));
