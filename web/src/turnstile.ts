// Browser vendor API only; this is not the server bootstrap/configuration DTO.
type ChallengeOptions = {
  size: "compact";
  sitekey: string;
  action: string;
  "response-field": false;
  "refresh-expired": "manual";
  "refresh-timeout": "manual";
  retry: "never";
  callback: (challenge: string) => void;
  "expired-callback": () => void;
  "timeout-callback": () => void;
  "error-callback": () => boolean;
};
type Turnstile = {
  render: (root: HTMLElement, options: ChallengeOptions) => string;
  remove: (id: string) => void;
};
declare global {
  interface Window {
    turnstile?: Turnstile;
  }
}
let loading: Promise<Turnstile> | undefined;
const apiURL =
  "https://challenges.cloudflare.com/turnstile/v0/api.js?render=explicit";
function load(): Promise<Turnstile> {
  if (window.turnstile && typeof window.turnstile.render === "function")
    return Promise.resolve(window.turnstile);
  if (loading) return loading;
  loading = new Promise<Turnstile>((resolve, reject) => {
    const script = document.createElement("script");
    script.src = apiURL;
    script.async = true;
    script.referrerPolicy = "no-referrer";
    const fail = () => {
      clearTimeout(timer);
      script.remove();
      loading = undefined;
      reject(new Error("Challenge unavailable"));
    };
    const timer = window.setTimeout(fail, 12000);
    script.onerror = fail;
    script.onload = () => {
      if (!window.turnstile || typeof window.turnstile.render !== "function") {
        fail();
        return;
      }
      clearTimeout(timer);
      resolve(window.turnstile);
    };
    document.head.append(script);
  });
  return loading;
}
export function mountChallenge(
  root: HTMLElement,
  siteKey: string,
  action: string,
  callbacks: {
    complete: (challenge: string) => void;
    expired: () => void;
    failed: () => void;
  },
): () => void {
  let disposed = false;
  let completed = false;
  let vendor: Turnstile | undefined;
  let id: string | undefined;
  const failure = () => {
    if (!disposed) callbacks.failed();
  };
  void load()
    .then((api) => {
      if (disposed) return;
      vendor = api;
      id = api.render(root, {
        size: "compact",
        sitekey: siteKey,
        action,
        "response-field": false,
        "refresh-expired": "manual",
        "refresh-timeout": "manual",
        retry: "never",
        callback: (challenge) => {
          if (disposed || completed) return;
          if (
            typeof challenge !== "string" ||
            !challenge ||
            challenge.length > 2048
          ) {
            failure();
            return;
          }
          completed = true;
          callbacks.complete(challenge);
        },
        "expired-callback": () => {
          if (!disposed && !completed) callbacks.expired();
        },
        "timeout-callback": () => {
          if (!disposed && !completed) callbacks.expired();
        },
        "error-callback": () => {
          failure();
          return true;
        },
      });
    })
    .catch(failure);
  return () => {
    disposed = true;
    if (vendor && id !== undefined) vendor.remove(id);
  };
}
