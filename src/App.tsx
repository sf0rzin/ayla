import { useCallback, useEffect, useMemo, useRef, useState, type FormEvent, type RefObject } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { getVersion } from "@tauri-apps/api/app";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { open } from "@tauri-apps/plugin-dialog";
import { relaunch } from "@tauri-apps/plugin-process";
import { check, type Update } from "@tauri-apps/plugin-updater";
import type { LucideIcon } from "lucide-react";
import {
  Activity,
  ArrowLeft,
  ArrowRight,
  Bike,
  Bot,
  Boxes,
  BrainCircuit,
  Camera,
  Check,
  ChevronDown,
  CarFront,
  CircleHelp,
  CircleDot,
  CircleGauge,
  Clapperboard,
  Clock3,
  Disc3,
  Download,
  Ellipsis,
  Eye,
  EyeOff,
  FileUp,
  Flower2,
  FolderOpen,
  Gem,
  Gift,
  Headphones,
  House,
  Info,
  LayoutGrid,
  ListFilter,
  ListChecks,
  LoaderCircle,
  LogOut,
  MessagesSquare,
  Minus,
  Music2,
  Network,
  PanelLeftClose,
  PanelLeftOpen,
  Play,
  Plus,
  Radio,
  RefreshCw,
  Search,
  Settings2,
  ShieldCheck,
  ShoppingBag,
  Sparkles,
  Square,
  StopCircle,
  Trash2,
  TrendingUp,
  UserPlus,
  Users,
  X,
} from "lucide-react";
import { ModuleBrandIcon } from "./ModuleBrandIcon";
import loginArtwork from "./assets/ayla-login-art.webp";
import {
  AuthApiError,
  login,
  logout,
  registerAccount,
  type AuthSession,
  type AuthUser,
} from "./authApi";
import "./App.css";

const MAX_PROXY_IMPORT_BYTES = 16 * 1024 * 1024;
const DEFAULT_MAX_SCAN_DIRECTORIES = 1_000;
const DEFAULT_MAX_SCAN_FILES = 10_000;
const DEFAULT_SCAN_BUDGET_MIB = 512;

type Page = "overview" | "modules" | "tasks" | "proxies" | "settings";
type ResourcePhase = "loading" | "ready" | "error";

interface AppOverview {
  version: string;
  modulesTotal: number;
  defaultThreads: number;
  proxiesTotal: number;
  proxiesLive: number;
  storagePath: string;
}

interface ModuleInfo {
  id: string;
  name: string;
  category: string;
  description: string;
  enabled: boolean;
}

interface AppSettings {
  threads: number;
  moduleThreads: Record<string, number>;
  delayMs: number;
  timeoutMs: number;
  retries: number;
  maxScanDirectories: number | null;
  maxScanFiles: number | null;
  scanBudgetMib: number | null;
  autoCheckUpdates: boolean;
  autoInstallUpdates: boolean;
}

type UpdatePhase = "idle" | "checking" | "current" | "available" | "downloading" | "ready" | "installing" | "restarting" | "blocked" | "error" | "unsupported";

interface AppUpdateState {
  phase: UpdatePhase;
  version: string;
  notes: string;
  publishedAt: string;
  downloadedBytes: number;
  totalBytes: number | null;
  downloadComplete: boolean;
  message: string;
}

interface UpdateInstallReadiness {
  canInstall: boolean;
  taskRunning: boolean;
  proxyCheckRunning: boolean;
  blockedReason: string | null;
}

const initialUpdateState: AppUpdateState = {
  phase: "idle",
  version: "",
  notes: "",
  publishedAt: "",
  downloadedBytes: 0,
  totalBytes: null,
  downloadComplete: false,
  message: "Updates are signed and delivered through public GitHub Releases.",
};

// Memoria de tentativas de instalacao automatica, FORA do processo.
// Um useRef morre junto com o app, e o instalador em modo passive mata e reabre
// o app — entao um release mal etiquetado (latest.json anuncia 2.0.1 mas o
// instalador entrega 2.0.0) viraria um laco infinito de baixar-instalar-morrer,
// sem janela pratica para alguem desligar a preferencia. Com o registro em
// disco, a segunda tentativa frustrada para o caminho automatico e devolve o
// controle ao botao manual.
const UPDATE_ATTEMPTS_KEY = "ayla.update-attempts";
const MAX_AUTO_INSTALL_ATTEMPTS = 2;

interface UpdateAttemptRecord {
  version: string;
  attempts: number;
}

function readUpdateAttempts(): UpdateAttemptRecord | null {
  try {
    const raw = localStorage.getItem(UPDATE_ATTEMPTS_KEY);
    if (!raw) return null;
    const parsed = JSON.parse(raw) as Partial<UpdateAttemptRecord>;
    if (typeof parsed?.version !== "string" || typeof parsed?.attempts !== "number") return null;
    return { version: parsed.version, attempts: parsed.attempts };
  } catch {
    return null;
  }
}

function autoInstallAllowedFor(version: string): boolean {
  const record = readUpdateAttempts();
  if (!record || record.version !== version) return true;
  return record.attempts < MAX_AUTO_INSTALL_ATTEMPTS;
}

function recordAutoInstallAttempt(version: string): void {
  try {
    const record = readUpdateAttempts();
    const attempts = record && record.version === version ? record.attempts + 1 : 1;
    localStorage.setItem(UPDATE_ATTEMPTS_KEY, JSON.stringify({ version, attempts }));
  } catch {
    // Sem localStorage o caminho automatico perde a rede de seguranca; nao vale
    // derrubar a aplicacao por isso.
  }
}

function clearUpdateAttempts(installedVersion: string): void {
  const record = readUpdateAttempts();
  if (record && record.version === installedVersion) {
    try { localStorage.removeItem(UPDATE_ATTEMPTS_KEY); } catch { /* idem */ }
  }
}

const updatePhaseLabels: Record<UpdatePhase, string> = {
  idle: "Ready",
  checking: "Checking",
  current: "Up to date",
  available: "Available",
  downloading: "Downloading",
  ready: "Ready to install",
  installing: "Installing",
  restarting: "Restarting",
  blocked: "Waiting",
  error: "Needs attention",
  unsupported: "Desktop only",
};

interface SystemMetrics {
  cpuPercent: number;
  cpuCount: number;
  memoryUsedBytes: number;
  memoryTotalBytes: number;
}

interface ProxyItem {
  id: string;
  display: string;
  protocol: string;
  host: string;
  port: number;
  hasAuth: boolean;
  status: "pending" | "live" | "httpOnly" | "failed";
  capability: "unchecked" | "httpsVerified" | "httpOnly" | "unavailable";
  latencyMs: number;
  message: string;
  ip: string;
  country: string;
  countryCode: string;
  city: string;
  createdAt: string;
  checkedAt: string;
}

interface AddProxiesResult {
  added: number;
  duplicates: number;
  rejected: Array<{ line: number; reason: string }>;
  total: number;
  items: ProxyItem[];
}

interface ProxyParseReport {
  accepted: Array<{
    protocol: string;
    host: string;
    port: number;
    hasAuth: boolean;
    display: string;
  }>;
  rejected: Array<{ line: number; reason: string }>;
  duplicates: number;
}

interface ProxyProgress {
  done: number;
  total: number;
  percent: number;
  live: number;
  httpOnly: number;
  failed: number;
  removed: number;
  id: string;
  item: ProxyItem | null;
  status: "running" | "live" | "httpOnly" | "failed" | "stopped" | "done";
  running: boolean;
}
interface ChatGptTaskSummary {
  active: number;
  authenticatedUnknown?: number;
  planUnavailable?: number;
  dead: number;
  rateLimited: number;
  errors: number;
  invalid: number;
  free: number;
  go: number;
  plus: number;
  pro: number;
  team: number;
  enterprise: number;
}

interface ModuleTaskSummary {
  active: number;
  authenticatedUnknown?: number;
  noEntitlement?: number;
  dead: number;
  rateLimited: number;
  errors: number;
  invalid: number;
  plans: Record<string, number>;
}


interface TaskSnapshot {
  runId: string;
  moduleId: string;
  status: string;
  total: number;
  discovered: number;
  locallyFiltered: number;
  discoveryComplete?: boolean;
  discoveryError?: string | null;
  queued: number;
  running: number;
  succeeded: number;
  failed: number;
  skipped: number;
  retried: number;
  percent: number;
  concurrency: number;
  requestedConcurrency: number;
  delayMs: number;
  useProxy: boolean;
  proxyCount: number;
  sequence: number;
  startedAt: string | number;
  finishedAt: string | number | null;
  historyPersisted?: boolean | null;
  resultsExportEnabled?: boolean;
  exportedActive?: number;
  exportedFailed?: number;
  exportErrors?: number;
  moduleSummary?: ModuleTaskSummary | null;
  chatgpt?: ChatGptTaskSummary | null;
}

interface TaskHistoryEntry {
  runId: string;
  moduleId: string;
  status: string;
  total: number;
  discovered?: number;
  locallyFiltered?: number;
  discoveryError?: string | null;
  succeeded: number;
  failed: number;
  skipped: number;
  concurrency: number;
  requestedConcurrency?: number;
  delayMs: number;
  useProxy?: boolean;
  proxyCount?: number;
  startedAt: string | number;
  finishedAt: string | number | null;
  durationMs?: number;
  resultsExportEnabled?: boolean;
  exportedActive?: number;
  exportedFailed?: number;
  exportErrors?: number;
}

const nav: Array<{ id: Page; label: string; icon: LucideIcon }> = [
  { id: "overview", label: "Overview", icon: House },
  { id: "modules", label: "Modules", icon: Boxes },
  { id: "tasks", label: "Tasks", icon: ListChecks },
  { id: "proxies", label: "Proxies", icon: Network },
  { id: "settings", label: "Settings", icon: Settings2 },
];

const pageMeta: Record<Page, { title: string; icon: LucideIcon }> = {
  overview: { title: "Overview", icon: CircleGauge },
  modules: { title: "Modules", icon: Boxes },
  tasks: { title: "Tasks", icon: ListChecks },
  proxies: { title: "Proxies", icon: Network },
  settings: { title: "Settings", icon: Settings2 },
};

const fallbackOverview: AppOverview = {
  version: "Preview",
  modulesTotal: 15,
  defaultThreads: 24,
  proxiesTotal: 0,
  proxiesLive: 0,
  storagePath: "Local app data",
};

const fallbackModules: ModuleInfo[] = [
  ["chatgpt", "ChatGPT", "AI", "AI assistant and productivity platform by OpenAI"],
  ["grok", "Grok", "AI", "AI assistant and real-time search platform by xAI"],
  ["tiktok", "TikTok", "Social", "Short-form video and social media platform"],
  ["zai", "zAI", "AI", "AI models and assistant services by Zhipu AI"],
  ["doordash", "DoorDash", "Marketplace", "Local delivery and commerce platform"],
  ["uber", "Uber", "Marketplace", "Mobility, delivery, and transportation platform"],
  ["sephora", "Sephora", "Marketplace", "Beauty retail and loyalty platform"],
  ["stockx", "StockX", "Marketplace", "Marketplace for sneakers, streetwear, and collectibles"],
  ["airbnb", "Airbnb", "Marketplace", "Marketplace for stays and travel experiences"],
  ["spotify", "Spotify", "Entertainment", "Music, podcasts, and audio streaming platform"],
  ["twitch", "Twitch", "Entertainment", "Live streaming for gaming, creators, and communities"],
  ["max", "HBO Max", "Entertainment", "HBO, Warner Bros., movies, series, and live events"],
  ["kick", "Kick", "Social", "Live-streaming and creator platform"],
  ["instagram", "Instagram", "Social", "Photo, video, and social networking platform"],
  ["reddit", "Reddit", "Social", "Community discussion and content-sharing platform"],
].map(([id, name, category, description]) => ({ id, name, category, description, enabled: id === "chatgpt" || id === "twitch" || id === "max" }));

const overviewPreviewEnabled = import.meta.env.DEV
  && typeof window !== "undefined"
  && new URLSearchParams(window.location.search).get("preview") === "overview";

const overviewPreviewSession: AuthSession = {
  token: "development-overview-preview",
  expiresAt: new Date(0).toISOString(),
  user: {
    id: "development-overview-preview",
    name: "Ayla Preview",
    email: "preview@ayla.local",
    role: "admin",
  },
};

const overviewPreviewHistory: TaskHistoryEntry[] = [
  [0, "chatgpt", 140, 136],
  [0, "twitch", 82, 79],
  [0, "max", 64, 61],
  [2, "chatgpt", 118, 114],
  [5, "twitch", 92, 89],
  [5, "chatgpt", 153, 149],
  [9, "max", 76, 72],
  [13, "chatgpt", 121, 118],
  [18, "twitch", 98, 96],
  [18, "max", 68, 64],
  [27, "chatgpt", 132, 128],
  [34, "twitch", 88, 85],
  [46, "chatgpt", 105, 102],
  [59, "max", 73, 70],
  [74, "chatgpt", 147, 142],
  [96, "twitch", 91, 87],
  [124, "chatgpt", 116, 113],
  [157, "max", 69, 66],
  [203, "chatgpt", 129, 125],
  [248, "twitch", 84, 81],
  [301, "chatgpt", 112, 108],
].map(([daysAgo, moduleId, total, succeeded], index) => {
  const finishedAt = new Date();
  finishedAt.setDate(finishedAt.getDate() - Number(daysAgo));
  finishedAt.setHours(12, 0, 0, 0);
  const startedAt = new Date(finishedAt.getTime() - 72_000);
  return {
    runId: `overview-preview-${index}`,
    moduleId: String(moduleId),
    status: "completed",
    total: Number(total),
    succeeded: Number(succeeded),
    failed: Number(total) - Number(succeeded),
    skipped: 0,
    concurrency: 24,
    delayMs: 120,
    startedAt: startedAt.toISOString(),
    finishedAt: finishedAt.toISOString(),
  };
});

function hasTauriRuntime() {
  return typeof window !== "undefined" && "__TAURI_INTERNALS__" in window;
}

function userInitials(name: string) {
  return name
    .split(/\s+/)
    .filter(Boolean)
    .slice(0, 2)
    .map((part) => part[0]?.toUpperCase())
    .join("") || "AY";
}

function userRoleLabel(user: AuthUser) {
  return user.role === "admin" ? "Administrator" : "Member";
}

function App() {
  const [authSession, setAuthSession] = useState<AuthSession | null>(() => (
    overviewPreviewEnabled ? overviewPreviewSession : null
  ));
  const [authenticationReady, setAuthenticationReady] = useState(() => (
    overviewPreviewEnabled || !import.meta.env.DEV || !hasTauriRuntime()
  ));
  const [developmentSessionActive, setDevelopmentSessionActive] = useState(false);

  useEffect(() => {
    if (overviewPreviewEnabled || !import.meta.env.DEV || !hasTauriRuntime()) return;
    let active = true;
    void invoke<AuthSession | null>("development_login_bypass_session")
      .then((session) => {
        if (active && import.meta.env.DEV && session) {
          setDevelopmentSessionActive(true);
          setAuthSession(session);
        }
      })
      .catch(() => undefined)
      .finally(() => {
        if (active) setAuthenticationReady(true);
      });
    return () => { active = false; };
  }, []);

  if (!authenticationReady) {
    return (
      <div className="login-screen login-bootstrap" role="status" aria-live="polite">
        <LoaderCircle className="is-spinning" size={20} />
        <span>Starting Ayla…</span>
      </div>
    );
  }

  if (!authSession) {
    return <LoginPreview onAuthenticated={setAuthSession} />;
  }

  const handleLogout = () => {
    const token = authSession.token;
    setAuthSession(null);
    setDevelopmentSessionActive(false);
    if (!overviewPreviewEnabled && !developmentSessionActive) {
      void logout(token).catch(() => undefined);
    }
  };

  return <WorkspaceApp user={authSession.user} onLogout={handleLogout} />;
}

function authErrorMessage(code: string) {
  switch (code) {
    case "INVALID_CREDENTIALS": return "Email or password is incorrect.";
    case "ACCOUNT_PENDING": return "Your account is waiting for administrator activation.";
    case "ACCOUNT_DISABLED": return "This account has been disabled.";
    case "INVALID_NAME": return "Enter a valid name.";
    case "INVALID_EMAIL": return "Enter a valid email address.";
    case "INVALID_PASSWORD": return "Use a password with at least 12 characters.";
    case "RATE_LIMITED": return "Too many attempts. Wait a moment and try again.";
    case "REQUEST_TIMEOUT": return "The server took too long to respond.";
    case "NETWORK_ERROR": return "Unable to reach the Ayla service.";
    default: return "Something went wrong. Try again.";
  }
}

function LoginPreview({ onAuthenticated }: { onAuthenticated: (session: AuthSession) => void }) {
  const [showPassword, setShowPassword] = useState(false);
  const [name, setName] = useState("");
  const [email, setEmail] = useState("");
  const [password, setPassword] = useState("");
  const [confirmPassword, setConfirmPassword] = useState("");
  const [mode, setMode] = useState<"signIn" | "register">("signIn");
  const [authState, setAuthState] = useState<"idle" | "loading" | "success" | "error">("idle");
  const [authMessage, setAuthMessage] = useState("");
  const [registrationPending, setRegistrationPending] = useState(false);
  const [invalidFields, setInvalidFields] = useState<Set<string>>(() => new Set());
  const nameInputRef = useRef<HTMLInputElement>(null);
  const emailInputRef = useRef<HTMLInputElement>(null);
  const passwordInputRef = useRef<HTMLInputElement>(null);
  const confirmPasswordInputRef = useRef<HTMLInputElement>(null);
  const matchedPasswordChecks = [
    password.length >= 12,
    /[a-z]/.test(password) && /[A-Z]/.test(password),
    /\d/.test(password),
    /[^A-Za-z0-9]/.test(password),
  ].filter(Boolean).length;
  const passwordScore = password ? Math.max(1, matchedPasswordChecks) : 0;
  const passwordLevel = ["Strength", "Weak", "Fair", "Good", "Strong"][passwordScore];
  const selectMode = (nextMode: "signIn" | "register") => {
    if (authState === "loading") return;
    setMode(nextMode);
    setAuthState("idle");
    setAuthMessage("");
    setRegistrationPending(false);
    setInvalidFields(new Set());
  };
  const handleAuthSubmit = async (event: FormEvent<HTMLFormElement>) => {
    event.preventDefault();
    if (authState === "loading" || authState === "success") return;

    setAuthMessage("");

    const emailValid = /^[^\s@]+@[^\s@]+\.[^\s@]+$/.test(email.trim());
    let validationError = "";
    let firstInvalid: "name" | "email" | "password" | "confirmPassword" | null = null;
    const nextInvalidFields = new Set<string>();
    if (!email.trim() || !password || (mode === "register" && (!name.trim() || !confirmPassword))) {
      validationError = "Complete all required fields.";
      if (mode === "register" && !name.trim()) {
        firstInvalid = "name";
        nextInvalidFields.add("name");
      }
      if (!email.trim()) {
        firstInvalid ??= "email";
        nextInvalidFields.add("email");
      }
      if (!password) {
        firstInvalid ??= "password";
        nextInvalidFields.add("password");
      }
      if (mode === "register" && !confirmPassword) {
        firstInvalid ??= "confirmPassword";
        nextInvalidFields.add("confirmPassword");
      }
    } else if (!emailValid) {
      validationError = "Enter a valid email address.";
      firstInvalid = "email";
      nextInvalidFields.add("email");
    } else if (mode === "register" && password.length < 12) {
      validationError = "Use a password with at least 12 characters.";
      firstInvalid = "password";
      nextInvalidFields.add("password");
    } else if (mode === "register" && passwordScore < 3) {
      validationError = "Choose a stronger password.";
      firstInvalid = "password";
      nextInvalidFields.add("password");
    } else if (mode === "register" && password !== confirmPassword) {
      validationError = "Passwords do not match.";
      firstInvalid = "confirmPassword";
      nextInvalidFields.add("confirmPassword");
    }

    if (validationError) {
      setInvalidFields(nextInvalidFields);
      setAuthMessage(validationError);
      setAuthState("error");
      const invalidRefs = {
        name: nameInputRef,
        email: emailInputRef,
        password: passwordInputRef,
        confirmPassword: confirmPasswordInputRef,
      };
      window.requestAnimationFrame(() => firstInvalid && invalidRefs[firstInvalid].current?.focus());
      return;
    }

    setInvalidFields(new Set());
    setAuthState("loading");
    const minimumAnimation = new Promise((resolve) => window.setTimeout(resolve, 650));

    try {
      if (mode === "register") {
        await Promise.all([
          registerAccount({ name: name.trim(), email: email.trim(), password }),
          minimumAnimation,
        ]);
        setAuthState("success");
        await new Promise((resolve) => window.setTimeout(resolve, 700));
        setRegistrationPending(true);
        setAuthState("idle");
      } else {
        const [session] = await Promise.all([
          login({ email: email.trim(), password }),
          minimumAnimation,
        ]);
        setAuthState("success");
        await new Promise((resolve) => window.setTimeout(resolve, 700));
        onAuthenticated(session);
      }
    } catch (error) {
      await minimumAnimation;
      const code = error instanceof AuthApiError ? error.code : "UNKNOWN";
      setAuthMessage(authErrorMessage(code));
      setAuthState("error");
      await new Promise((resolve) => window.setTimeout(resolve, 1_600));
      setAuthState("idle");
    }
  };
  const runWindowAction = (action: (window: ReturnType<typeof getCurrentWindow>) => Promise<void>) => {
    try {
      void action(getCurrentWindow()).catch(() => undefined);
    } catch {
      // Browser previews do not have a Tauri window.
    }
  };

  return (
    <div className="login-screen">
      <div className="login-drag-region" data-tauri-drag-region onDoubleClick={() => runWindowAction((window) => window.toggleMaximize())} />
      <div className="login-window-controls">
        <button type="button" onClick={() => runWindowAction((window) => window.minimize())} aria-label="Minimize"><Minus size={15} /></button>
        <button type="button" onClick={() => runWindowAction((window) => window.toggleMaximize())} aria-label="Maximize"><Square size={12} /></button>
        <button className="window-close" type="button" onClick={() => runWindowAction((window) => window.close())} aria-label="Close"><X size={15} /></button>
      </div>

      <div className="login-shell">
        <aside className="login-art-panel" aria-hidden="true">
          <div className="login-art-backdrop" />
          <img className="login-art-image" src={loginArtwork} alt="" draggable={false} />
          <div className="login-art-copy">
            <strong>Ayla</strong>
            <p>A Windows desktop application for authorized session validation.</p>
          </div>
        </aside>

        <main className="login-form-panel">
          <div className="login-form-wrap">
            {registrationPending ? (
              <section className="login-registration-pending" aria-labelledby="registration-pending-title">
                <span className="login-pending-icon"><Clock3 size={22} /></span>
                <h1 id="registration-pending-title">Account pending activation</h1>
                <p>An administrator must activate your account before you can sign in.</p>
                <button type="button" onClick={() => selectMode("signIn")}>Back to sign in <ArrowRight size={14} /></button>
              </section>
            ) : (
              <>
                <div className="login-mode-switch" aria-label="Authentication mode">
                  <button className={mode === "signIn" ? "active" : ""} type="button" onClick={() => selectMode("signIn")} disabled={authState === "loading"}>Sign in</button>
                  <button className={mode === "register" ? "active" : ""} type="button" onClick={() => selectMode("register")} disabled={authState === "loading"}>Register</button>
                </div>

                <header className="login-form-heading">
                  <h1>{mode === "signIn" ? "Sign in" : "Create account"}</h1>
                </header>

                <form className="login-form" onSubmit={handleAuthSubmit} noValidate>
                  {mode === "register" && (
                    <label className="login-field">
                      <span>Name</span>
                      <input ref={nameInputRef} type="text" name="name" autoComplete="name" placeholder="Your name" value={name} onChange={(event) => { setName(event.target.value); setInvalidFields((current) => { const next = new Set(current); next.delete("name"); return next; }); }} disabled={authState === "loading"} aria-invalid={invalidFields.has("name")} aria-describedby={invalidFields.has("name") ? "login-auth-message" : undefined} />
                    </label>
                  )}

                  <label className="login-field">
                    <span>Email address</span>
                    <input ref={emailInputRef} type="email" name="email" autoComplete="email" placeholder="you@example.com" value={email} onChange={(event) => { setEmail(event.target.value); setInvalidFields((current) => { const next = new Set(current); next.delete("email"); return next; }); }} disabled={authState === "loading"} aria-invalid={invalidFields.has("email")} aria-describedby={invalidFields.has("email") ? "login-auth-message" : undefined} />
                  </label>

                  <label className="login-field">
                    <span>Password</span>
                    <div className="login-password-input">
                      <input ref={passwordInputRef} type={showPassword ? "text" : "password"} name="password" autoComplete={mode === "register" ? "new-password" : "current-password"} placeholder="Enter your password" value={password} onChange={(event) => { setPassword(event.target.value); setInvalidFields((current) => { const next = new Set(current); next.delete("password"); return next; }); }} disabled={authState === "loading"} aria-invalid={invalidFields.has("password")} aria-describedby={invalidFields.has("password") ? "login-auth-message" : undefined} />
                      <button type="button" onClick={() => setShowPassword((current) => !current)} aria-label={showPassword ? "Hide password" : "Show password"} disabled={authState === "loading"}>
                        {showPassword ? <EyeOff size={15} /> : <Eye size={15} />}
                      </button>
                    </div>
                  </label>

                  {mode === "register" && (
                    <div className={`login-password-strength strength-${passwordScore}`}>
                      <div className="login-strength-track" role="meter" aria-label="Password strength" aria-valuemin={0} aria-valuemax={4} aria-valuenow={passwordScore} aria-valuetext={passwordLevel}>
                        {[1, 2, 3, 4].map((level) => <span className={passwordScore >= level ? "filled" : ""} key={level} />)}
                      </div>
                      <span>{passwordLevel}</span>
                    </div>
                  )}

                  {mode === "register" && (
                    <label className="login-field">
                      <span>Confirm password</span>
                      <input ref={confirmPasswordInputRef} type="password" name="confirmPassword" autoComplete="new-password" placeholder="Repeat your password" value={confirmPassword} onChange={(event) => { setConfirmPassword(event.target.value); setInvalidFields((current) => { const next = new Set(current); next.delete("confirmPassword"); return next; }); }} disabled={authState === "loading"} aria-invalid={invalidFields.has("confirmPassword")} aria-describedby={invalidFields.has("confirmPassword") ? "login-auth-message" : undefined} />
                    </label>
                  )}

                  <button className={`login-submit state-${authState}`} type="submit" disabled={authState === "loading" || authState === "success"}>
                    <span className="login-button-content" key={authState}>
                      {authState === "loading" && <><LoaderCircle className="login-button-spinner" size={15} /> {mode === "signIn" ? "Signing in" : "Creating account"}</>}
                      {authState === "success" && <><Check size={16} strokeWidth={2.4} /> Success</>}
                      {authState === "error" && <><X size={15} /> Failed</>}
                      {authState === "idle" && <>{mode === "signIn" ? "Sign in" : "Register"} <ArrowRight size={15} /></>}
                    </span>
                  </button>
                  {authMessage && <span className="login-auth-message" id="login-auth-message" role="alert">{authMessage}</span>}
                </form>
              </>
            )}
          </div>
        </main>
      </div>
    </div>
  );
}

function WorkspaceApp({ user, onLogout }: { user: AuthUser; onLogout: () => void }) {
  const [navigation, setNavigation] = useState<{ entries: Page[]; index: number }>({ entries: ["overview"], index: 0 });
  const [sidebarOpen, setSidebarOpen] = useState(true);
  const [aboutOpen, setAboutOpen] = useState(false);
  const [overview, setOverview] = useState<AppOverview | null>(() => (
    overviewPreviewEnabled
      ? { ...fallbackOverview, proxiesTotal: 48, proxiesLive: 44 }
      : null
  ));
  const [appVersion, setAppVersion] = useState<string | null>(() => (
    overviewPreviewEnabled ? fallbackOverview.version : null
  ));
  const [proxyCounts, setProxyCounts] = useState(() => ({
    total: overviewPreviewEnabled ? 48 : 0,
    live: overviewPreviewEnabled ? 44 : 0,
  }));
  const [modules, setModules] = useState<ModuleInfo[]>(() => overviewPreviewEnabled ? fallbackModules : []);
  const [settings, setSettings] = useState<AppSettings | null>(null);
  const [resourcePhases, setResourcePhases] = useState<Record<"overview" | "modules" | "settings", ResourcePhase>>({
    overview: overviewPreviewEnabled ? "ready" : "loading",
    modules: overviewPreviewEnabled ? "ready" : "loading",
    settings: overviewPreviewEnabled ? "ready" : "loading",
  });
  const [resourceErrors, setResourceErrors] = useState<Partial<Record<"overview" | "modules" | "settings", string>>>({});
  const [settingsRecoveryNotice, setSettingsRecoveryNotice] = useState("");
  const [systemMetrics, setSystemMetrics] = useState<SystemMetrics | null>(() => (
    overviewPreviewEnabled
      ? { cpuPercent: 23, cpuCount: 8, memoryUsedBytes: 7_730_941_132, memoryTotalBytes: 17_179_869_184 }
      : null
  ));
  const [taskSnapshot, setTaskSnapshot] = useState<TaskSnapshot | null>(null);
  const [taskHistory, setTaskHistory] = useState<TaskHistoryEntry[]>(() => (
    overviewPreviewEnabled ? overviewPreviewHistory : []
  ));
  const [taskHistoryPhase, setTaskHistoryPhase] = useState<ResourcePhase>(overviewPreviewEnabled ? "ready" : "loading");
  const [taskHistoryError, setTaskHistoryError] = useState("");
  const taskHistoryRequestRef = useRef(0);
  const [configuredModuleId, setConfiguredModuleId] = useState<string | null>(null);
  const [updateState, setUpdateState] = useState<AppUpdateState>(initialUpdateState);
  const pendingUpdateRef = useRef<Update | null>(null);
  const updateDownloadedRef = useRef(false);
  const updateBusyRef = useRef(false);
  const autoCheckAttemptedRef = useRef(false);
  // Versao ja armada nesta sessao, para o efeito nao reentrar.
  const autoInstallArmedRef = useRef("");
  // A decisao e congelada no momento da checagem, nao lida do valor corrente da
  // preferencia. Dois motivos: o botao "Check manually" promete "without
  // installing it", entao so a checagem automatica instala; e ligar o toggle
  // depois nao pode instalar retroativamente um update ja pendente.
  const lastCheckRef = useRef({ automatic: false, autoInstall: false });
  // Espelho das settings para ser lido de dentro de callbacks com deps vazias.
  const settingsRef = useRef<AppSettings | null>(null);
  // Contagem regressiva visivel antes do reinicio, com opcao de adiar.
  const [pendingRestart, setPendingRestart] = useState<{ version: string; secondsLeft: number } | null>(null);
  const [modulePreferences, setModulePreferences] = useState<Record<string, boolean>>(() => {
    try {
      return JSON.parse(localStorage.getItem("ayla.module-preferences") ?? "{}") as Record<string, boolean>;
    } catch {
      return {};
    }
  });
  const searchInputRef = useRef<HTMLInputElement>(null);
  const aboutDialogRef = useRef<HTMLElement>(null);
  const aboutReturnFocusRef = useRef<HTMLElement | null>(null);
  const page = navigation.entries[navigation.index];
  const meta = pageMeta[page];

  const navigate = (nextPage: Page) => {
    if (nextPage !== "settings") setConfiguredModuleId(null);
    setNavigation((current) => {
      if (current.entries[current.index] === nextPage) return current;
      const entries = [...current.entries.slice(0, current.index + 1), nextPage];
      return { entries, index: entries.length - 1 };
    });
  };

  const goBack = () => setNavigation((current) => ({ ...current, index: Math.max(0, current.index - 1) }));
  const goForward = () => setNavigation((current) => ({ ...current, index: Math.min(current.entries.length - 1, current.index + 1) }));
  const focusSearch = () => {
    setSidebarOpen(true);
    window.setTimeout(() => searchInputRef.current?.focus(), 0);
  };

  const closeAbout = useCallback(() => {
    setAboutOpen(false);
    const returnFocus = aboutReturnFocusRef.current;
    aboutReturnFocusRef.current = null;
    window.requestAnimationFrame(() => returnFocus?.focus());
  }, []);

  const openAbout = () => {
    aboutReturnFocusRef.current = document.activeElement instanceof HTMLElement ? document.activeElement : null;
    setAboutOpen(true);
  };

  const checkForUpdates = useCallback(async (automatic = false) => {
    if (updateBusyRef.current) return;
    if (!hasTauriRuntime()) {
      if (!automatic) {
        setUpdateState((current) => ({
          ...current,
          phase: "unsupported",
          message: "Update checks are available in the installed desktop app.",
        }));
      }
      return;
    }

    updateBusyRef.current = true;
    lastCheckRef.current = {
      automatic,
      autoInstall: Boolean(settingsRef.current?.autoInstallUpdates),
    };
    setUpdateState((current) => ({
      ...current,
      phase: "checking",
      message: automatic ? "Checking for updates…" : "Contacting the update service…",
    }));

    try {
      const previous = pendingUpdateRef.current;
      pendingUpdateRef.current = null;
      updateDownloadedRef.current = false;
      if (previous) await previous.close().catch(() => undefined);

      const next = await check({ timeout: 15_000 });
      if (!next) {
        setUpdateState({
          ...initialUpdateState,
          phase: "current",
          message: "Ayla is up to date.",
        });
        return;
      }

      pendingUpdateRef.current = next;
      setUpdateState({
        phase: "available",
        version: next.version,
        notes: next.body?.trim() ?? "",
        publishedAt: next.date ?? "",
        downloadedBytes: 0,
        totalBytes: null,
        downloadComplete: false,
        message: `Ayla ${next.version} is available.`,
      });
    } catch (reason: unknown) {
      setUpdateState({
        ...initialUpdateState,
        phase: "error",
        message: `Unable to check for updates: ${String(reason)}`,
      });
    } finally {
      updateBusyRef.current = false;
    }
  }, []);

  const installUpdate = useCallback(async () => {
    if (updateBusyRef.current) return;
    const pending = pendingUpdateRef.current;
    if (!pending) {
      setUpdateState((current) => ({
        ...current,
        phase: "error",
        message: "Check for updates again before installing.",
      }));
      return;
    }

    updateBusyRef.current = true;
    let gateArmed = false;
    try {
      if (!updateDownloadedRef.current) {
        let downloadedBytes = 0;
        let totalBytes: number | null = null;
        setUpdateState((current) => ({
          ...current,
          phase: "downloading",
          downloadedBytes: 0,
          totalBytes: null,
          downloadComplete: false,
          message: `Downloading Ayla ${pending.version}…`,
        }));

        try {
          await pending.download((event) => {
            if (event.event === "Started") {
              totalBytes = event.data.contentLength ?? null;
              downloadedBytes = 0;
            } else if (event.event === "Progress") {
              downloadedBytes += event.data.chunkLength;
            }

            setUpdateState((current) => ({
              ...current,
              downloadedBytes,
              totalBytes,
            }));
          }, { timeout: 120_000 });
        } catch (reason: unknown) {
          updateDownloadedRef.current = false;
          setUpdateState((current) => ({
            ...current,
            phase: "error",
            downloadedBytes: 0,
            totalBytes: null,
            downloadComplete: false,
            message: `Unable to download the update: ${String(reason)}`,
          }));
          return;
        }

        updateDownloadedRef.current = true;
        setUpdateState((current) => ({
          ...current,
          phase: "ready",
          downloadComplete: true,
          message: "Download complete. Preparing to install…",
        }));
      }

      const readiness = await invoke<UpdateInstallReadiness>("prepare_update_install");
      if (!readiness.canInstall) {
        // Bloqueio NAO e falha: o download ja esta em disco e installUpdate()
        // pula essa etapa numa proxima tentativa. Reagenda a contagem em vez de
        // desistir da sessao inteira, que era o defeito anterior.
        setUpdateState((current) => ({
          ...current,
          phase: "blocked",
          message: `Update downloaded. ${readiness.blockedReason ?? "Finish active work before installing."} Ayla will try again shortly.`,
        }));
        if (lastCheckRef.current.autoInstall && pendingUpdateRef.current) {
          setPendingRestart({ version: pendingUpdateRef.current.version, secondsLeft: 300 });
        }
        return;
      }
      gateArmed = true;

      setUpdateState((current) => ({
        ...current,
        phase: "installing",
        message: "Installing the signed update…",
      }));
      await pending.install();
      setUpdateState((current) => ({
        ...current,
        phase: "restarting",
        message: "Update installed. Restarting Ayla…",
      }));
      await relaunch();
    } catch (reason: unknown) {
      if (gateArmed) await invoke("release_update_install_gate").catch(() => undefined);
      setUpdateState((current) => ({
        ...current,
        phase: "error",
        message: `Unable to install the update: ${String(reason)}`,
      }));
    } finally {
      updateBusyRef.current = false;
    }
  }, []);

  const refreshTaskHistory = useCallback(async () => {
    const requestId = ++taskHistoryRequestRef.current;
    if (overviewPreviewEnabled) {
      setTaskHistoryPhase("ready");
      return;
    }
    setTaskHistoryPhase("loading");
    try {
      const nextHistory = await invoke<TaskHistoryEntry[]>("task_history", { limit: 100 });
      if (requestId !== taskHistoryRequestRef.current) return;
      setTaskHistory(nextHistory);
      setTaskHistoryError("");
      setTaskHistoryPhase("ready");
    } catch (reason: unknown) {
      if (!overviewPreviewEnabled && requestId === taskHistoryRequestRef.current) {
        setTaskHistoryError(String(reason));
        setTaskHistoryPhase("error");
      }
    }
  }, []);

  const acceptTaskSnapshot = useCallback((snapshot: TaskSnapshot) => {
    setTaskSnapshot((current) => {
      if (current?.runId === snapshot.runId && current.sequence > snapshot.sequence) return current;
      if (current && current.runId !== snapshot.runId && taskIsRunning(current)) return current;
      return snapshot;
    });
  }, []);

  const loadBootstrap = useCallback(async () => {
    if (overviewPreviewEnabled) return;
    setResourcePhases({ overview: "loading", modules: "loading", settings: "loading" });
    setResourceErrors({});
    const [overviewResult, modulesResult, settingsResult, versionResult] = await Promise.allSettled([
      invoke<AppOverview>("get_app_overview"),
      invoke<ModuleInfo[]>("list_modules"),
      invoke<AppSettings>("get_settings"),
      getVersion(),
    ]);

    if (overviewResult.status === "fulfilled") {
      setOverview(overviewResult.value);
      setProxyCounts({ total: overviewResult.value.proxiesTotal, live: overviewResult.value.proxiesLive });
    }
    if (modulesResult.status === "fulfilled") setModules(modulesResult.value);
    if (settingsResult.status === "fulfilled") setSettings(settingsResult.value);
    if (versionResult.status === "fulfilled") setAppVersion(versionResult.value);
    setResourcePhases({
      overview: overviewResult.status === "fulfilled" ? "ready" : "error",
      modules: modulesResult.status === "fulfilled" ? "ready" : "error",
      settings: settingsResult.status === "fulfilled" ? "ready" : "error",
    });
    setResourceErrors({
      ...(overviewResult.status === "rejected" ? { overview: String(overviewResult.reason) } : {}),
      ...(modulesResult.status === "rejected" ? { modules: String(modulesResult.reason) } : {}),
      ...(settingsResult.status === "rejected" ? { settings: String(settingsResult.reason) } : {}),
    });
    if (settingsResult.status === "fulfilled") {
      const notice = await invoke<string | null>("get_settings_recovery_notice").catch(() => null);
      setSettingsRecoveryNotice(notice ?? "");
    }
  }, []);

  useEffect(() => {
    void loadBootstrap();
  }, [loadBootstrap]);

  useEffect(() => {
    if (!settings?.autoCheckUpdates || autoCheckAttemptedRef.current) return;
    autoCheckAttemptedRef.current = true;
    void checkForUpdates(true);
  }, [checkForUpdates, settings?.autoCheckUpdates]);

  useEffect(() => {
    settingsRef.current = settings ?? null;
  }, [settings]);

  // Quando a versao instalada finalmente alcanca a que foi tentada, o registro
  // e limpo. Isso tambem e o detector do release mal etiquetado: se a versao
  // nunca alcanca, o contador nao zera e o caminho automatico para sozinho.
  useEffect(() => {
    if (appVersion) clearUpdateAttempts(appVersion);
  }, [appVersion]);

  // Atualizacao sem pedir aprovacao, mas NUNCA reiniciando de surpresa: o efeito
  // apenas arma uma contagem regressiva visivel no shell. Baixar so comeca
  // quando ela termina, entao adiar tambem poupa banda. A instalacao em si passa
  // pelo mesmo installUpdate() do botao, logo assinatura e portao continuam
  // valendo.
  useEffect(() => {
    if (!lastCheckRef.current.automatic || !lastCheckRef.current.autoInstall) return;
    if (updateState.phase !== "available" || !updateState.version) return;
    if (autoInstallArmedRef.current === updateState.version) return;
    if (!autoInstallAllowedFor(updateState.version)) return;
    autoInstallArmedRef.current = updateState.version;
    recordAutoInstallAttempt(updateState.version);
    setPendingRestart({ version: updateState.version, secondsLeft: 60 });
  }, [updateState.phase, updateState.version]);

  useEffect(() => {
    if (!pendingRestart) return;
    if (pendingRestart.secondsLeft <= 0) {
      setPendingRestart(null);
      void installUpdate();
      return;
    }
    const timer = setTimeout(() => {
      setPendingRestart((current) => (current ? { ...current, secondsLeft: current.secondsLeft - 1 } : null));
    }, 1_000);
    return () => clearTimeout(timer);
  }, [installUpdate, pendingRestart]);

  useEffect(() => () => {
    const pending = pendingUpdateRef.current;
    pendingUpdateRef.current = null;
    if (pending) void pending.close().catch(() => undefined);
  }, []);

  useEffect(() => {
    localStorage.setItem("ayla.module-preferences", JSON.stringify(modulePreferences));
  }, [modulePreferences]);

  useEffect(() => {
    const onKeyDown = (event: KeyboardEvent) => {
      if ((event.ctrlKey || event.metaKey) && event.key.toLowerCase() === "k") {
        event.preventDefault();
        focusSearch();
      }
    };
    window.addEventListener("keydown", onKeyDown);
    return () => window.removeEventListener("keydown", onKeyDown);
  }, []);

  useEffect(() => {
    if (!aboutOpen) return;
    const dialog = aboutDialogRef.current;
    if (!dialog) return;
    const focusable = () => [...dialog.querySelectorAll<HTMLElement>('button, [href], input, select, textarea, [tabindex]:not([tabindex="-1"])')]
      .filter((element) => !element.hasAttribute("disabled"));
    window.requestAnimationFrame(() => (focusable()[0] ?? dialog).focus());
    const trapFocus = (event: KeyboardEvent) => {
      if (event.key === "Escape") {
        event.preventDefault();
        closeAbout();
        return;
      }
      if (event.key !== "Tab") return;
      const elements = focusable();
      if (elements.length === 0) {
        event.preventDefault();
        dialog.focus();
        return;
      }
      const first = elements[0];
      const last = elements[elements.length - 1];
      if (event.shiftKey && document.activeElement === first) {
        event.preventDefault();
        last.focus();
      } else if (!event.shiftKey && document.activeElement === last) {
        event.preventDefault();
        first.focus();
      }
    };
    window.addEventListener("keydown", trapFocus);
    return () => window.removeEventListener("keydown", trapFocus);
  }, [aboutOpen, closeAbout]);

  useEffect(() => {
    if (page !== "overview") return;
    let active = true;
    let timer = 0;
    const refresh = async () => {
      try {
        const nextMetrics = await invoke<SystemMetrics>("get_system_metrics");
        if (active) setSystemMetrics(nextMetrics);
      } catch {
        // Keep the preview usable outside Tauri.
      }
    };
    void refresh();
    timer = window.setInterval(() => void refresh(), 2_500);
    return () => {
      active = false;
      window.clearInterval(timer);
    };
  }, [page]);

  useEffect(() => {
    let active = true;
    const cleanups: Array<() => void> = [];
    const receive = (snapshot: TaskSnapshot) => {
      if (active) acceptTaskSnapshot(snapshot);
    };

    const initialize = async () => {
      const registrations = await Promise.allSettled([
        listen<TaskSnapshot>("task:progress", ({ payload }) => receive(payload)),
        listen<TaskSnapshot>("task:done", ({ payload }) => {
          receive(payload);
          void refreshTaskHistory();
        }),
      ]);
      registrations.forEach((registration) => {
        if (registration.status === "fulfilled") {
          if (active) cleanups.push(registration.value);
          else registration.value();
        }
      });
      if (!active) return;
      const activeTasks = await invoke<TaskSnapshot[]>("list_tasks").catch(() => []);
      const current = activeTasks.find(taskIsRunning) ?? activeTasks[0] ?? null;
      if (current) receive(current);
      await refreshTaskHistory();
    };

    void initialize();
    return () => {
      active = false;
      cleanups.splice(0).forEach((cleanup) => cleanup());
    };
  }, [acceptTaskSnapshot, refreshTaskHistory]);

  const runnableModules = useMemo(
    () => modules.map((item) => ({ ...item, enabled: item.enabled && (modulePreferences[item.id] ?? true) })),
    [modulePreferences, modules],
  );
  const workbenchClass = sidebarOpen ? "workbench" : "workbench sidebar-collapsed";

  return (
    <div className="desktop-frame">
      <Titlebar
        activePage={meta.title}
        sidebarOpen={sidebarOpen}
        canGoBack={navigation.index > 0}
        canGoForward={navigation.index < navigation.entries.length - 1}
        onToggleSidebar={() => setSidebarOpen((current) => !current)}
        onBack={goBack}
        onForward={goForward}
        onNavigate={navigate}
        onSearch={focusSearch}
        onAbout={openAbout}
      />
      <div className={workbenchClass}>
        {sidebarOpen && (
          <Sidebar
            page={page}
            onNavigate={navigate}
            overview={overview}
            modules={modules}
            taskSnapshot={taskSnapshot}
            searchInputRef={searchInputRef}
            user={user}
            onLogout={onLogout}
          />
        )}

        <div className="main-column">
          {pendingRestart && (
            <div className="notice" role="status" aria-live="polite">
              <Download size={15} />
              <div>
                <strong>Ayla {pendingRestart.version} will be installed</strong>
                <span>
                  Ayla restarts in {pendingRestart.secondsLeft}s and you will need to sign in again. Save anything you typed.
                </span>
              </div>
              <button
                className="button small"
                type="button"
                onClick={() => {
                  setPendingRestart(null);
                  void installUpdate();
                }}
              >
                Restart now
              </button>
              <button className="button outline small" type="button" onClick={() => setPendingRestart(null)}>
                Not now
              </button>
            </div>
          )}
          {(updateState.phase === "downloading" || updateState.phase === "installing" || updateState.phase === "restarting") && (
            <div className="notice" role="status" aria-live="polite">
              <LoaderCircle className="is-spinning" size={15} />
              <div>
                <strong>{updatePhaseLabels[updateState.phase]}</strong>
                <span>{updateState.message}</span>
              </div>
            </div>
          )}
          <main className={`content page-${page}`}>
            {(["overview", "modules", "settings"] as const).some((resource) => resource === page && resourcePhases[resource] === "loading") && (
              <div className="notice" role="status">
                <LoaderCircle className="is-spinning" size={15} />
                <div><strong>Loading local data</strong><span>Ayla is reading the saved {page} state.</span></div>
              </div>
            )}
            {(["overview", "modules", "settings"] as const).some((resource) => resource === page && resourcePhases[resource] === "error") && (
              <div className="notice danger-notice" title={resourceErrors[page as "overview" | "modules" | "settings"]}>
                <CircleDot size={15} />
                <div><strong>{pageMeta[page].title} could not be loaded</strong><span>The saved data was not replaced with an empty state.</span></div>
                <button className="button outline small" type="button" onClick={() => void loadBootstrap()}>Retry</button>
              </div>
            )}
            {page === "overview" && taskHistoryPhase === "error" && (
              <div className="notice danger-notice" title={taskHistoryError}>
                <CircleDot size={15} />
                <div><strong>Recent activity could not be loaded</strong><span>Existing history remains on disk. Retry the read.</span></div>
                <button className="button outline small" type="button" onClick={() => void refreshTaskHistory()}>Retry</button>
              </div>
            )}
            {page === "settings" && settingsRecoveryNotice && (
              <div className="notice" role="status">
                <Info size={15} />
                <div><strong>Settings recovery needs review</strong><span>{settingsRecoveryNotice}</span></div>
              </div>
            )}
            <Overview active={page === "overview" && resourcePhases.overview === "ready"} overview={overview} metrics={systemMetrics} history={taskHistory} modules={modules} user={user} />
            <Modules
                active={page === "modules" && resourcePhases.modules === "ready"}
                modules={modules}
                preferences={modulePreferences}
                onToggle={(id) => setModulePreferences((current) => ({ ...current, [id]: !(current[id] ?? true) }))}
                onConfigure={(id) => { setConfiguredModuleId(id); navigate("settings"); }}
              />
            <Tasks
                active={page === "tasks" && resourcePhases.modules === "ready"}
                modules={runnableModules}
                defaultConcurrency={settings?.threads ?? 24}
                moduleConcurrency={settings?.moduleThreads}
                defaultDelayMs={settings?.delayMs ?? 120}
                proxiesLive={proxyCounts.live}
                onOpenProxies={() => navigate("proxies")}
                onTaskSnapshot={acceptTaskSnapshot}
                onHistoryChanged={refreshTaskHistory}
              />
            <Proxies
                active={page === "proxies"}
                defaultThreads={settings?.threads ?? 24}
                defaultTimeoutMs={settings?.timeoutMs ?? 15_000}
                onCountsChanged={(proxiesTotal, proxiesLive) => {
                  setProxyCounts({ total: proxiesTotal, live: proxiesLive });
                  setOverview((current) => current ? { ...current, proxiesTotal, proxiesLive } : current);
                }}
              />
            <Settings
                active={page === "settings" && resourcePhases.settings === "ready"}
                settings={settings}
                configuredModule={modules.find((item) => item.id === configuredModuleId) ?? null}
                installedVersion={appVersion ?? overview?.version ?? "Unavailable"}
                updateState={updateState}
                onCheckForUpdates={() => void checkForUpdates(false)}
                onInstallUpdate={() => void installUpdate()}
                onSaved={(nextSettings) => { setSettings(nextSettings); setSettingsRecoveryNotice(""); }}
              />
          </main>
        </div>

      </div>

      {aboutOpen && (
        <div className="modal-backdrop" role="presentation" onMouseDown={closeAbout}>
          <article ref={aboutDialogRef} className="about-dialog" role="dialog" aria-modal="true" aria-labelledby="about-title" tabIndex={-1} onMouseDown={(event) => event.stopPropagation()}>
            <div className="about-mark"><Flower2 size={22} /></div>
            <h2 id="about-title">Ayla</h2>
            <p>Version {appVersion ?? overview?.version ?? "Unavailable"}</p>
            <button className="button secondary" type="button" onClick={closeAbout}>Close</button>
          </article>
        </div>
      )}
    </div>
  );
}

function Titlebar({
  activePage,
  sidebarOpen,
  canGoBack,
  canGoForward,
  onToggleSidebar,
  onBack,
  onForward,
  onNavigate,
  onSearch,
  onAbout,
}: {
  activePage: string;
  sidebarOpen: boolean;
  canGoBack: boolean;
  canGoForward: boolean;
  onToggleSidebar: () => void;
  onBack: () => void;
  onForward: () => void;
  onNavigate: (page: Page) => void;
  onSearch: () => void;
  onAbout: () => void;
}) {
  const [openMenu, setOpenMenu] = useState<"file" | "edit" | "view" | "help" | null>(null);
  const menuRoot = useRef<HTMLDivElement>(null);
  const runWindowAction = (action: (window: ReturnType<typeof getCurrentWindow>) => Promise<void>) => {
    try {
      void action(getCurrentWindow()).catch(() => undefined);
    } catch {
      // Browser previews do not have a Tauri window.
    }
  };

  useEffect(() => {
    const closeFromOutside = (event: PointerEvent) => {
      if (!menuRoot.current?.contains(event.target as Node)) setOpenMenu(null);
    };
    const closeFromKeyboard = (event: KeyboardEvent) => {
      if (event.key === "Escape") setOpenMenu(null);
    };
    window.addEventListener("pointerdown", closeFromOutside);
    window.addEventListener("keydown", closeFromKeyboard);
    return () => {
      window.removeEventListener("pointerdown", closeFromOutside);
      window.removeEventListener("keydown", closeFromKeyboard);
    };
  }, []);

  const choose = (action: () => void) => {
    setOpenMenu(null);
    action();
  };

  return (
    <header className="titlebar">
      <div className="titlebar-navigation">
        <button className="titlebar-icon" type="button" onClick={onToggleSidebar} aria-label={sidebarOpen ? "Hide sidebar" : "Show sidebar"} title={sidebarOpen ? "Hide sidebar" : "Show sidebar"}>
          {sidebarOpen ? <PanelLeftClose size={16} /> : <PanelLeftOpen size={16} />}
        </button>
        <button className="titlebar-icon" type="button" onClick={onBack} disabled={!canGoBack} aria-label="Go back" title="Go back"><ArrowLeft size={16} /></button>
        <button className="titlebar-icon" type="button" onClick={onForward} disabled={!canGoForward} aria-label="Go forward" title="Go forward"><ArrowRight size={16} /></button>
      </div>
      <div className="titlebar-menu" aria-label="Application menu" ref={menuRoot}>
        <div className="titlebar-menu-group">
          <button className={openMenu === "file" ? "titlebar-menu-button active" : "titlebar-menu-button"} type="button" aria-expanded={openMenu === "file"} onClick={() => setOpenMenu((current) => current === "file" ? null : "file")}>File</button>
          {openMenu === "file" && <div className="titlebar-dropdown" aria-label="File actions"><button type="button" onClick={() => choose(() => onNavigate("overview"))}>Open Overview</button><button type="button" onClick={() => choose(() => onNavigate("tasks"))}>New Task</button><span className="menu-separator" /><button type="button" onClick={() => choose(() => runWindowAction((window) => window.close()))}>Exit Ayla</button></div>}
        </div>
        <div className="titlebar-menu-group">
          <button className={openMenu === "edit" ? "titlebar-menu-button active" : "titlebar-menu-button"} type="button" aria-expanded={openMenu === "edit"} onClick={() => setOpenMenu((current) => current === "edit" ? null : "edit")}>Edit</button>
          {openMenu === "edit" && <div className="titlebar-dropdown" aria-label="Edit actions"><button type="button" onClick={() => choose(onSearch)}>Search <kbd>Ctrl K</kbd></button><button type="button" onClick={() => choose(() => onNavigate("settings"))}>Settings</button></div>}
        </div>
        <div className="titlebar-menu-group">
          <button className={openMenu === "view" ? "titlebar-menu-button active" : "titlebar-menu-button"} type="button" aria-expanded={openMenu === "view"} onClick={() => setOpenMenu((current) => current === "view" ? null : "view")}>View</button>
          {openMenu === "view" && <div className="titlebar-dropdown" aria-label="View actions"><button type="button" onClick={() => choose(onToggleSidebar)}>{sidebarOpen ? "Hide" : "Show"} Sidebar</button></div>}
        </div>
        <div className="titlebar-menu-group">
          <button className={openMenu === "help" ? "titlebar-menu-button active" : "titlebar-menu-button"} type="button" aria-expanded={openMenu === "help"} onClick={() => setOpenMenu((current) => current === "help" ? null : "help")}>Help</button>
          {openMenu === "help" && <div className="titlebar-dropdown" aria-label="Help actions"><button type="button" onClick={() => choose(onAbout)}><Info size={14} /> About Ayla</button></div>}
        </div>
      </div>
      <div className="titlebar-drag" data-tauri-drag-region onDoubleClick={() => runWindowAction((window) => window.toggleMaximize())}>
        <span data-tauri-drag-region>{activePage}</span>
      </div>
      <div className="window-controls">
        <button type="button" onClick={() => runWindowAction((window) => window.minimize())} aria-label="Minimize"><Minus size={15} /></button>
        <button type="button" onClick={() => runWindowAction((window) => window.toggleMaximize())} aria-label="Maximize"><Square size={12} /></button>
        <button className="window-close" type="button" onClick={() => runWindowAction((window) => window.close())} aria-label="Close"><X size={15} /></button>
      </div>
    </header>
  );
}

function Sidebar({
  page,
  onNavigate,
  overview,
  modules,
  taskSnapshot,
  searchInputRef,
  user,
  onLogout,
}: {
  page: Page;
  onNavigate: (page: Page) => void;
  overview: AppOverview | null;
  modules: ModuleInfo[];
  taskSnapshot: TaskSnapshot | null;
  searchInputRef: RefObject<HTMLInputElement | null>;
  user: AuthUser;
  onLogout: () => void;
}) {
  const [query, setQuery] = useState("");
  const [searchOpen, setSearchOpen] = useState(false);
  const [profilePopover, setProfilePopover] = useState<"profile" | "help" | null>(null);
  const [planExpanded, setPlanExpanded] = useState(false);
  const [helpDetail, setHelpDetail] = useState<"gift" | "shortcuts" | "help" | null>(null);
  const [profileMessage, setProfileMessage] = useState("");
  const profileRoot = useRef<HTMLDivElement>(null);
  const profileTriggerRef = useRef<HTMLButtonElement>(null);
  const helpTriggerRef = useRef<HTMLButtonElement>(null);
  const profilePopoverRef = useRef<HTMLDivElement>(null);
  const normalizedQuery = query.trim().toLowerCase();
  const pageResults = normalizedQuery ? nav.filter((item) => item.label.toLowerCase().includes(normalizedQuery)) : [];
  const moduleResults = normalizedQuery ? modules.filter((item) => `${item.name} ${item.category}`.toLowerCase().includes(normalizedQuery)).slice(0, 5) : [];
  const progress = Math.max(0, Math.min(100, taskSnapshot?.percent ?? 0));
  const taskDone = taskSnapshot ? Math.max(0, taskSnapshot.total - taskSnapshot.queued - taskSnapshot.running) : 0;
  const taskDiscovering = taskIsDiscovering(taskSnapshot);
  const initials = userInitials(user.name);
  const roleLabel = userRoleLabel(user);

  const openFirstResult = () => {
    if (pageResults[0]) onNavigate(pageResults[0].id);
    else if (moduleResults[0]) onNavigate("modules");
    else return;
    setQuery("");
    setSearchOpen(false);
  };

  useEffect(() => {
    const closeFromOutside = (event: PointerEvent) => {
      if (!profileRoot.current?.contains(event.target as Node)) {
        setProfilePopover(null);
        setHelpDetail(null);
      }
    };
    const closeFromKeyboard = (event: KeyboardEvent) => {
      if (event.key === "Escape" && profilePopover) {
        const returnFocus = profilePopover === "help" ? helpTriggerRef.current : profileTriggerRef.current;
        setProfilePopover(null);
        setHelpDetail(null);
        window.requestAnimationFrame(() => returnFocus?.focus());
      }
    };
    window.addEventListener("pointerdown", closeFromOutside);
    window.addEventListener("keydown", closeFromKeyboard);
    return () => {
      window.removeEventListener("pointerdown", closeFromOutside);
      window.removeEventListener("keydown", closeFromKeyboard);
    };
  }, [profilePopover]);

  useEffect(() => {
    if (!profilePopover) return;
    window.requestAnimationFrame(() => profilePopoverRef.current?.querySelector<HTMLButtonElement>("button")?.focus());
  }, [profilePopover]);

  const copyInvitation = async () => {
    try {
      await navigator.clipboard.writeText("I’d like to invite you to try Ayla with me.");
      setProfileMessage("Invitation message copied.");
    } catch {
      setProfileMessage("Clipboard access is unavailable.");
    }
  };

  return (
    <aside className="sidebar">
      <div className="sidebar-wordmark">
        <strong>Ayla</strong>
      </div>
      <div className="sidebar-search" onBlur={(event) => {
        if (!event.currentTarget.contains(event.relatedTarget as Node | null)) setSearchOpen(false);
      }}>
        <Search size={14} />
        <input
          ref={searchInputRef}
          value={query}
          onChange={(event) => setQuery(event.target.value)}
          onFocus={() => setSearchOpen(true)}
          onKeyDown={(event) => {
            if (event.key === "Enter") openFirstResult();
            if (event.key === "Escape") {
              setQuery("");
              setSearchOpen(false);
              event.currentTarget.blur();
            }
          }}
          placeholder="Search"
          aria-label="Search pages and modules"
        />
        <kbd>Ctrl K</kbd>
        {searchOpen && normalizedQuery && (
          <div className="search-results">
            {pageResults.map((item) => {
              const Icon = item.icon;
              return <button type="button" key={item.id} onMouseDown={(event) => event.preventDefault()} onClick={() => { onNavigate(item.id); setQuery(""); setSearchOpen(false); }}><Icon size={14} /><span>{item.label}</span><small>Page</small></button>;
            })}
            {moduleResults.map((item) => {
              const Icon = moduleIcons[item.id] ?? Boxes;
              return <button type="button" key={item.id} onMouseDown={(event) => event.preventDefault()} onClick={() => { onNavigate("modules"); setQuery(""); setSearchOpen(false); }}><Icon size={14} /><span>{item.name}</span><small>{item.category}</small></button>;
            })}
            {pageResults.length === 0 && moduleResults.length === 0 && <div className="search-empty">No results</div>}
          </div>
        )}
      </div>

      <nav className="sidebar-nav">
        {nav.map((item) => {
          const Icon = item.icon;
          return (
            <button className={page === item.id ? "nav-item active" : "nav-item"} key={item.id} onClick={() => onNavigate(item.id)} type="button">
              <Icon size={16} strokeWidth={1.7} />
              <span>{item.label}</span>
              {item.id === "modules" && <small>{overview?.modulesTotal ?? 15}</small>}
              {item.id === "proxies" && Boolean(overview?.proxiesTotal) && <small>{overview?.proxiesTotal}</small>}
            </button>
          );
        })}
      </nav>

      <div className="sidebar-spacer" />
      <div className="sidebar-runtime">
        <div><Activity size={14} /><span>Task progress</span><strong>{taskDiscovering ? "Scanning" : `${Math.round(progress)}%`}</strong></div>
        {taskDiscovering ? (
          <div className="runtime-track indeterminate" role="progressbar" aria-label="Discovering task files"><span /></div>
        ) : (
          <div className="runtime-track" role="progressbar" aria-label="Task progress" aria-valuemin={0} aria-valuemax={100} aria-valuenow={Math.round(progress)}><span style={{ width: `${progress}%` }} /></div>
        )}
        <small>{taskSnapshot ? taskDiscovering ? `${taskSnapshot.discovered.toLocaleString("en-US")} scanned · Discovering` : `${taskDone} of ${taskSnapshot.total} · ${taskStatusLabel(taskSnapshot.status)}` : "No active task"}</small>
      </div>
      <div className="sidebar-profile-wrap" ref={profileRoot}>
        {profilePopover === "profile" && (
          <div className="profile-popover" ref={profilePopoverRef} aria-label="Profile actions">
            <div className="profile-popover-header"><span className="avatar">{initials}</span><div><strong>{user.name}</strong><small>{user.email}</small></div><span className="profile-plan-badge">{roleLabel}</span></div>
            <button className="profile-menu-item" type="button" aria-expanded={planExpanded} onClick={() => setPlanExpanded((current) => !current)}><CircleGauge size={16} /><span>Plan information</span><ChevronDown className={planExpanded ? "expanded" : ""} size={15} /></button>
            {planExpanded && (
              <div className="profile-plan-details">
                <div className="profile-plan-current"><span>Account access</span><strong>{roleLabel}</strong></div>
                <div className="profile-plan-grid">
                  <div><span>Time remaining</span><strong>—</strong></div>
                  <div><span>Next billing</span><strong>Unavailable</strong></div>
                  <div><span>Next charge</span><strong>—</strong></div>
                </div>
                <button type="button" onClick={() => setProfileMessage("Billing details will appear when an account service is connected.")}>Learn more</button>
              </div>
            )}
            <button className="profile-menu-item" type="button" onClick={() => void copyInvitation()}><UserPlus size={16} /><span>Invite friend</span></button>
            <span className="profile-menu-separator" />
            <button className="profile-menu-item danger" type="button" onClick={onLogout}><LogOut size={16} /><span>Log out</span></button>
            {profileMessage && <p className="profile-menu-message" role="status">{profileMessage}</p>}
          </div>
        )}

        {profilePopover === "help" && (
          <div className="profile-popover profile-help-popover" ref={profilePopoverRef} aria-label="Help actions">
            <div className="profile-help-heading"><strong>Help & extras</strong><small>Ayla</small></div>
            <button className="profile-menu-item" type="button" onClick={() => setHelpDetail("gift")}><Gift size={16} /><span>Send gift</span></button>
            <button className="profile-menu-item" type="button" onClick={() => setHelpDetail("shortcuts")}><ListChecks size={16} /><span>Keyboard shortcuts</span></button>
            <button className="profile-menu-item" type="button" onClick={() => setHelpDetail("help")}><CircleHelp size={16} /><span>Help</span></button>
            {helpDetail === "gift" && <div className="profile-help-detail"><strong>Send gift</strong><p>Gift purchases will be available after account billing is connected. No charge will be made here.</p></div>}
            {helpDetail === "shortcuts" && <div className="profile-help-detail shortcut-detail"><strong>Keyboard shortcuts</strong><div><span>Search</span><kbd>Ctrl K</kbd></div><div><span>Close menus</span><kbd>Esc</kbd></div></div>}
            {helpDetail === "help" && <div className="profile-help-detail"><strong>Quick help</strong><p>Use Tasks to run validations, Modules to choose platforms, and Proxies to manage network routes.</p></div>}
          </div>
        )}

        <div className="sidebar-profile">
          <button className="profile-trigger" ref={profileTriggerRef} type="button" aria-haspopup="dialog" aria-expanded={profilePopover === "profile"} onClick={() => { setProfileMessage(""); setProfilePopover((current) => current === "profile" ? null : "profile"); setHelpDetail(null); }}>
            <span className="avatar">{initials}</span>
            <span><strong>{user.name}</strong><small>{roleLabel}</small></span>
          </button>
          <button className="profile-help-trigger" ref={helpTriggerRef} type="button" aria-label="Open help menu" aria-haspopup="dialog" aria-expanded={profilePopover === "help"} onClick={() => { setProfilePopover((current) => current === "help" ? null : "help"); setHelpDetail(null); }}><CircleHelp size={17} /></button>
        </div>
      </div>
    </aside>
  );
}

function Overview({ active, overview, metrics, history, modules, user }: { active: boolean; overview: AppOverview | null; metrics: SystemMetrics | null; history: TaskHistoryEntry[]; modules: ModuleInfo[]; user: AuthUser }) {
  const memoryPercent = metrics?.memoryTotalBytes ? (metrics.memoryUsedBytes / metrics.memoryTotalBytes) * 100 : 0;
  const sessionsChecked = history.reduce((sum, item) => sum + item.total, 0);
  const sessionsSucceeded = history.reduce((sum, item) => sum + item.succeeded, 0);
  const successRate = sessionsChecked ? (sessionsSucceeded / sessionsChecked) * 100 : null;
  const proxyAvailability = overview?.proxiesTotal ? ((overview.proxiesLive / overview.proxiesTotal) * 100) : null;
  const averageTaskSize = history.length ? sessionsChecked / history.length : null;
  const today = new Intl.DateTimeFormat("en-US", { month: "long", day: "numeric", year: "numeric" }).format(new Date());
  const initials = userInitials(user.name);
  const roleLabel = userRoleLabel(user);

  const moduleUsage = useMemo(() => {
    const counts = new Map<string, number>();
    history.forEach((item) => counts.set(item.moduleId, (counts.get(item.moduleId) ?? 0) + 1));
    const names = new Map(modules.map((item) => [item.id, item.name]));
    return [...counts.entries()]
      .map(([id, runs]) => ({ id, name: names.get(id) ?? id, runs }))
      .sort((left, right) => right.runs - left.runs || left.name.localeCompare(right.name))
      .slice(0, 4);
  }, [history, modules]);

  return (
    <section className="overview-page" hidden={!active}>
      <header className="profile-hero">
        <div className="profile-avatar" aria-hidden="true">{initials}</div>
        <div className="profile-identity">
          <h1>{user.name}</h1>
          <p>{user.email} <span className="profile-plan">{roleLabel}</span></p>
        </div>
        <span className="profile-date">{today}</span>
      </header>

      <div className="profile-stats" aria-label="Profile statistics">
        <ProfileStat value={history.length} label="Recent tasks" />
        <ProfileStat value={sessionsChecked} label="Sessions checked" />
        <ProfileStat value={successRate === null ? "—" : `${successRate.toFixed(0)}%`} label="Success rate" />
        <ProfileStat value={metrics ? `${metrics.cpuPercent.toFixed(0)}%` : "—"} label="CPU usage" />
        <ProfileStat value={metrics ? `${memoryPercent.toFixed(0)}%` : "—"} label="Memory usage" />
      </div>

      <ContributionActivity history={history} />

      <div className="overview-insights-grid">
        <section className="insight-panel" aria-labelledby="activity-insights-title">
          <h2 id="activity-insights-title">Activity insights</h2>
          <div className="insight-list">
            <InsightRow label="Proxy availability" value={proxyAvailability === null ? "—" : `${proxyAvailability.toFixed(0)}%`} />
            <InsightRow label="Average task size" value={averageTaskSize === null ? "—" : `${averageTaskSize.toFixed(1)} sessions`} />
            <InsightRow label="Validation success rate" value={successRate === null ? "—" : `${successRate.toFixed(1)}%`} />
          </div>
        </section>

        <section className="insight-panel" aria-labelledby="most-used-modules-title">
          <h2 id="most-used-modules-title">Most used modules</h2>
          {moduleUsage.length ? (
            <div className="module-usage-list">
              {moduleUsage.map((item) => (
                <div className="module-usage-row" key={item.id}>
                  <span className="module-usage-icon"><ModuleBrandIcon id={item.id} /></span>
                  <span className="module-usage-copy">{item.name}</span>
                  <span>{item.runs} {item.runs === 1 ? "run" : "runs"}</span>
                </div>
              ))}
            </div>
          ) : <p className="insight-empty">No module activity yet.</p>}
        </section>
      </div>
    </section>
  );
}

function ProfileStat({ value, label }: { value: string | number; label: string }) {
  return <div className="profile-stat"><strong>{value}</strong><span>{label}</span></div>;
}

function InsightRow({ label, value }: { label: string; value: string }) {
  return <div className="insight-row"><span>{label}</span><strong>{value}</strong></div>;
}

function localDayKey(date: Date) {
  const month = String(date.getMonth() + 1).padStart(2, "0");
  const day = String(date.getDate()).padStart(2, "0");
  return `${date.getFullYear()}-${month}-${day}`;
}

function ContributionActivity({ history }: { history: TaskHistoryEntry[] }) {
  const { weeks, start, end } = useMemo(() => {
    const today = new Date();
    today.setHours(12, 0, 0, 0);
    const windowEnd = new Date(today);
    windowEnd.setDate(windowEnd.getDate() + (6 - windowEnd.getDay()));
    const windowStart = new Date(windowEnd);
    windowStart.setDate(windowStart.getDate() - ((53 * 7) - 1));
    const cursor = new Date(windowStart);
    const result: Date[][] = [];
    for (let weekIndex = 0; weekIndex < 53; weekIndex += 1) {
      const week: Date[] = [];
      for (let day = 0; day < 7; day += 1) {
        week.push(new Date(cursor));
        cursor.setDate(cursor.getDate() + 1);
      }
      result.push(week);
    }
    return { weeks: result, start: windowStart, end: windowEnd };
  }, []);

  const activity = useMemo(() => {
    const counts = new Map<string, number>();
    history.forEach((item) => {
      const date = new Date(item.finishedAt ?? item.startedAt);
      if (Number.isNaN(date.getTime()) || date < start || date > end) return;
      const key = localDayKey(date);
      counts.set(key, (counts.get(key) ?? 0) + 1);
    });
    return counts;
  }, [end, history, start]);

  const monthLabels = useMemo(() => weeks.map((week, index) => {
    const marker = index === 0 ? week[0] : week.find((date) => date.getDate() === 1);
    return marker ? new Intl.DateTimeFormat("en-US", { month: "short" }).format(marker) : "";
  }), [weeks]);
  const total = [...activity.values()].reduce((sum, count) => sum + count, 0);
  const maximum = Math.max(1, ...activity.values());

  return (
    <article className="card contribution-card">
      <div className="section-heading">
        <div>
          <h2>Task activity</h2>
          <span>Latest 100 recorded tasks · 12-month window</span>
        </div>
        <span className="contribution-summary"><strong>{total}</strong> {total === 1 ? "task" : "tasks"}</span>
      </div>
      <div className="contribution-scroll">
        <div className="heatmap-months">
          {monthLabels.map((label, index) => <span key={`${label}-${index}`}>{label}</span>)}
        </div>
        <div className="heatmap-body">
          <div className="heatmap-weekdays" aria-hidden="true">
            <span />
            <span>Mon</span>
            <span />
            <span>Wed</span>
            <span />
            <span>Fri</span>
            <span />
          </div>
          <div className="heatmap-grid" role="img" aria-label={`${total} of the latest 100 recorded ${total === 1 ? "task" : "tasks"}, placed in a 12-month window`}>
            {weeks.flatMap((week) => week.map((date) => {
              const count = activity.get(localDayKey(date)) ?? 0;
              const level = count === 0 ? 0 : Math.max(1, Math.ceil((count / maximum) * 4));
              const label = `${new Intl.DateTimeFormat("en-US", { month: "short", day: "numeric", year: "numeric" }).format(date)}: ${count} ${count === 1 ? "task" : "tasks"}`;
              return <span className={`heatmap-cell level-${level}`} key={localDayKey(date)} title={label} aria-label={label} />;
            }))}
          </div>
        </div>
        <div className="heatmap-legend" aria-hidden="true">
          <span>Less</span>
          {[0, 1, 2, 3, 4].map((level) => <i className={`heatmap-cell level-${level}`} key={level} />)}
          <span>More</span>
        </div>
      </div>
    </article>
  );
}

const moduleIcons: Record<string, LucideIcon> = {
  chatgpt: Bot,
  grok: BrainCircuit,
  zai: Sparkles,
  tiktok: Music2,
  kick: Radio,
  instagram: Camera,
  reddit: MessagesSquare,
  doordash: Bike,
  uber: CarFront,
  sephora: Gem,
  stockx: TrendingUp,
  airbnb: House,
  spotify: Disc3,
  twitch: Radio,
  max: Clapperboard,
};

const moduleFilters: Array<{ id: string; label: string; icon: LucideIcon }> = [
  { id: "all", label: "All", icon: LayoutGrid },
  { id: "ai", label: "AI", icon: Bot },
  { id: "social", label: "Social", icon: Users },
  { id: "marketplace", label: "Marketplaces", icon: ShoppingBag },
  { id: "entertainment", label: "Entertainment", icon: Headphones },
];

const moduleTopics: Array<{ id: string; title: string; modules: string[] }> = [
  { id: "featured", title: "Featured", modules: ["chatgpt", "grok", "tiktok", "zai"] },
  { id: "commerce", title: "Commerce & services", modules: ["doordash", "uber", "sephora", "stockx", "airbnb"] },
  { id: "media", title: "Media & communities", modules: ["spotify", "twitch", "max", "kick", "instagram", "reddit"] },
];

function Modules({ active, modules, preferences, onToggle, onConfigure }: { active: boolean; modules: ModuleInfo[]; preferences: Record<string, boolean>; onToggle: (id: string) => void; onConfigure: (id: string) => void }) {
  const [filter, setFilter] = useState("all");
  const [query, setQuery] = useState("");
  const [filtersOpen, setFiltersOpen] = useState(false);
  const installedModules = useMemo(() => modules.filter((item) => item.enabled), [modules]);
  const visibleModules = useMemo(() => {
    const normalizedQuery = query.trim().toLowerCase();
    return modules.filter((item) => {
      const matchesFilter = filter === "all" || item.category.toLowerCase() === filter;
      const matchesQuery = !normalizedQuery || `${item.name} ${item.category} ${item.description}`.toLowerCase().includes(normalizedQuery);
      return matchesFilter && matchesQuery;
    });
  }, [filter, modules, query]);
  const activeFilter = moduleFilters.find((item) => item.id === filter)?.label ?? "Filter";
  const groupedModules = useMemo(() => {
    const byId = new Map(visibleModules.map((item) => [item.id, item]));
    const groups = moduleTopics.map((topic) => ({
      ...topic,
      items: topic.modules.map((id) => byId.get(id)).filter((item): item is ModuleInfo => Boolean(item)),
    })).filter((topic) => topic.items.length > 0);
    const knownIds = new Set(moduleTopics.flatMap((topic) => topic.modules));
    const other = visibleModules.filter((item) => !knownIds.has(item.id));
    if (other.length > 0) groups.push({ id: "other", title: "Other", modules: other.map((item) => item.id), items: other });
    return groups;
  }, [visibleModules]);

  return (
    <section className="modules-page" hidden={!active}>
      <header className="module-page-header">
        <h1>Modules</h1>
      </header>

      <div className="module-search-shell">
        <Search size={17} />
        <input type="search" value={query} onChange={(event) => setQuery(event.target.value)} placeholder="Search modules" aria-label="Search modules" />
        <button className={filter === "all" ? "module-filter-pill" : "module-filter-pill active"} type="button" aria-expanded={filtersOpen} onClick={() => setFiltersOpen((current) => !current)}>
          <ListFilter size={14} />{filter === "all" ? "Filter" : activeFilter}
        </button>
      </div>

      {filtersOpen && (
        <div className="module-filter-panel" aria-label="Module categories">
          {moduleFilters.map((item) => {
            const Icon = item.icon;
            return <button className={filter === item.id ? "active" : ""} type="button" aria-pressed={filter === item.id} key={item.id} onClick={() => { setFilter(item.id); setFiltersOpen(false); }}><Icon size={14} />{item.label}</button>;
          })}
        </div>
      )}

      <section className="installed-modules" aria-labelledby="installed-modules-title">
        <div className="module-section-heading"><h2 id="installed-modules-title">Installed</h2><span>{installedModules.length}</span></div>
        <div className="installed-module-strip">
          {installedModules.map((item) => (
            <button type="button" key={item.id} title={item.name} aria-label={`Show ${item.name}`} onClick={() => { setFilter("all"); setFiltersOpen(false); setQuery(item.name); }}>
              <ModuleBrandIcon id={item.id} />
            </button>
          ))}
          {installedModules.length === 0 && <span>No installed modules.</span>}
        </div>
      </section>

      <section className="module-catalog" aria-label="Module catalog">
        <div className="module-catalog-summary"><span>Browse modules</span><strong>{visibleModules.length}</strong></div>
        {groupedModules.map((topic) => (
          <section className="module-topic" aria-labelledby={`module-topic-${topic.id}`} key={topic.id}>
            <div className="module-topic-heading"><div><h2 id={`module-topic-${topic.id}`}>{topic.title}</h2></div><span>{topic.items.length}</span></div>
            <div className="module-catalog-grid">
              {topic.items.map((item) => {
                const checked = item.enabled && (preferences[item.id] ?? true);
                return (
                  <article className={`module-list-item${item.enabled ? "" : " unavailable"}`} key={item.id}>
                    <div className={`module-icon brand-${item.id}`}><ModuleBrandIcon id={item.id} /></div>
                    <div className="module-list-copy"><div><h3>{item.name}</h3><span>{item.category}</span></div><p>{item.description}</p></div>
                    <div className="module-list-actions">
                      <button className="module-more" type="button" onClick={() => onConfigure(item.id)} disabled={!item.enabled} aria-label={`Open ${item.name} settings`} title={item.enabled ? `Open ${item.name} settings` : "Settings will be available with this module"}><Ellipsis size={18} /></button>
                      <button className={checked ? "switch-control active" : "switch-control"} type="button" role="switch" aria-checked={checked} aria-label={`${checked ? "Disable" : "Enable"} ${item.name}`} disabled={!item.enabled} onClick={() => onToggle(item.id)}><span /></button>
                    </div>
                  </article>
                );
              })}
            </div>
          </section>
        ))}
        {visibleModules.length === 0 && <div className="module-empty"><Search size={20} /><strong>No matching modules</strong><span>Try another search or category.</span></div>}
      </section>
    </section>
  );
}

const runningTaskStatuses = new Set(["queued", "running", "cancelling"]);
const taskDateFormatter = new Intl.DateTimeFormat("en-US", { dateStyle: "medium", timeStyle: "short" });

function taskIsRunning(snapshot: TaskSnapshot | null) {
  return Boolean(snapshot && runningTaskStatuses.has(snapshot.status.toLowerCase()));
}

function taskIsDiscovering(snapshot: TaskSnapshot | null) {
  return Boolean(snapshot && snapshot.discoveryComplete === false && taskIsRunning(snapshot));
}

function taskTone(status: string) {
  const normalized = status.toLowerCase();
  if (["completed", "complete", "succeeded", "success"].includes(normalized)) return "success";
  if (["cancelled", "canceled", "stopped"].includes(normalized)) return "warning";
  if (normalized === "failed") return "danger";
  return "running";
}

function taskStatusLabel(status: string) {
  const labels: Record<string, string> = {
    queued: "Queued",
    running: "Running",
    cancelling: "Cancelling",
    cancelled: "Cancelled",
    canceled: "Cancelled",
    stopped: "Stopped",
    completed: "Completed",
    complete: "Completed",
    succeeded: "Completed",
    success: "Completed",
    failed: "Failed",
  };
  return labels[status.toLowerCase()] ?? status;
}

function formatTaskDate(value: string | number | null) {
  if (!value) return "In progress";
  const date = new Date(value);
  return Number.isNaN(date.getTime()) ? value : taskDateFormatter.format(date);
}

function Tasks({ active, modules, defaultConcurrency, moduleConcurrency, defaultDelayMs, proxiesLive, onOpenProxies, onTaskSnapshot, onHistoryChanged }: { active: boolean; modules: ModuleInfo[]; defaultConcurrency: number; moduleConcurrency?: Record<string, number>; defaultDelayMs: number; proxiesLive: number; onOpenProxies: () => void; onTaskSnapshot: (snapshot: TaskSnapshot) => void; onHistoryChanged: () => void }) {
  const enabledModules = useMemo(() => modules.filter((module) => module.enabled), [modules]);
  const preferredModuleId = enabledModules.find((module) => module.id === "chatgpt")?.id ?? enabledModules[0]?.id ?? "";
  const [moduleId, setModuleId] = useState(preferredModuleId);
  const [rawEntries, setRawEntries] = useState("");
  const [concurrency, setConcurrency] = useState(Math.max(1, Math.min(32, defaultConcurrency)));
  const [delayMs, setDelayMs] = useState(defaultDelayMs);
  const [useProxy, setUseProxy] = useState(false);
  const [outputDirectory, setOutputDirectory] = useState("");
  const [selectingOutputDirectory, setSelectingOutputDirectory] = useState(false);
  const [selectingSources, setSelectingSources] = useState(false);
  const [draggingSources, setDraggingSources] = useState(false);
  const [activeTask, setActiveTask] = useState<TaskSnapshot | null>(null);
  const [history, setHistory] = useState<TaskHistoryEntry[]>([]);
  const [historyPhase, setHistoryPhase] = useState<ResourcePhase>("loading");
  const [historyError, setHistoryError] = useState("");
  const [starting, setStarting] = useState(false);
  const [cancelling, setCancelling] = useState(false);
  const [message, setMessage] = useState("");
  const [taskView, setTaskView] = useState<"history" | "create">("history");
  const [historyPage, setHistoryPage] = useState(0);
  const historyRequestRef = useRef(0);

  const moduleById = useMemo(() => new Map(modules.map((module) => [module.id, module])), [modules]);
  const entryCount = useMemo(() => rawEntries.split(/\r?\n/).filter((entry) => entry.trim()).length, [rawEntries]);
  const uniqueEntryCount = useMemo(() => new Set(rawEntries.split(/\r?\n/).map((entry) => entry.trim()).filter(Boolean)).size, [rawEntries]);
  const duplicateEntryCount = Math.max(0, entryCount - uniqueEntryCount);
  const entryLimitExceeded = entryCount > 10_000;
  const selectedModule = moduleById.get(moduleId);
  const outputFolderName = useMemo(() => {
    const normalized = outputDirectory.replace(/[\\/]+$/, "");
    return normalized.split(/[\\/]/).filter(Boolean).pop() ?? normalized;
  }, [outputDirectory]);
  const requestedConcurrency = Math.max(1, Math.min(32, Math.trunc(concurrency || 1)));
  const effectiveConcurrency = useProxy ? Math.min(requestedConcurrency, Math.max(1, proxiesLive)) : requestedConcurrency;
  const activeRunning = taskIsRunning(activeTask);
  const activeDiscovering = taskIsDiscovering(activeTask);
  const progress = Math.max(0, Math.min(100, activeTask?.percent ?? 0));
  const moduleSummary = activeTask?.moduleSummary;
  const chatgptSummary = activeTask?.chatgpt;
  const outcomeSummary = moduleSummary ?? chatgptSummary;
  const hasDetailedOutcome = Boolean(outcomeSummary);
  const planSummary = moduleSummary
    ? Object.entries(moduleSummary.plans)
        .filter(([, count]) => count > 0)
        .sort(([, left], [, right]) => right - left)
        .map(([plan, count]) => `${plan}: ${count}`)
        .join(" · ")
    : chatgptSummary
      ? [
        ["Free", chatgptSummary.free],
        ["Go", chatgptSummary.go],
        ["Plus", chatgptSummary.plus],
        ["Pro", chatgptSummary.pro],
        ["Team", chatgptSummary.team],
        ["Enterprise", chatgptSummary.enterprise],
      ].filter(([, count]) => Number(count) > 0).map(([plan, count]) => `${plan}: ${count}`).join(" · ")
      : "";
  const historyPageSize = 5;
  const historyPageCount = Math.max(1, Math.ceil(history.length / historyPageSize));
  const visibleHistory = history.slice(historyPage * historyPageSize, (historyPage + 1) * historyPageSize);


  function acceptSnapshot(next: TaskSnapshot) {
    onTaskSnapshot(next);
    setActiveTask((current) => {
      if (current && current.runId !== next.runId && taskIsRunning(current)) return current;
      if (current?.runId === next.runId && current.sequence > next.sequence) return current;
      return next;
    });
  }

  async function chooseOutputDirectory() {
    setSelectingOutputDirectory(true);
    setMessage("");
    try {
      const selected = await open({
        directory: true,
        multiple: false,
        title: "Choose results folder",
      });
      if (typeof selected === "string") setOutputDirectory(selected);
    } catch {
      setMessage("Ayla could not open the folder picker. Try again.");
    } finally {
      setSelectingOutputDirectory(false);
    }
  }

  async function chooseSources(kind: "files" | "folder") {
    setSelectingSources(true);
    setMessage("");
    try {
      const selected = await open({
        directory: kind === "folder",
        multiple: kind === "files",
        title: kind === "files" ? "Choose authorized session files" : "Choose an authorized folder",
      });
      const paths = (Array.isArray(selected) ? selected : typeof selected === "string" ? [selected] : [])
        .map((path) => path.trim())
        .filter(Boolean);
      if (paths.length > 0) {
        setRawEntries((current) => [...new Set([
          ...current.split(/\r?\n/).map((entry) => entry.trim()).filter(Boolean),
          ...paths,
        ])].join("\n"));
      }
    } catch {
      setMessage("Ayla could not open the source picker. You can still paste paths below.");
    } finally {
      setSelectingSources(false);
    }
  }

  async function reloadHistory() {
    const requestId = ++historyRequestRef.current;
    setHistoryPhase("loading");
    try {
      const nextHistory = await invoke<TaskHistoryEntry[]>("task_history", { limit: 30 });
      if (requestId !== historyRequestRef.current) return;
      setHistory(nextHistory);
      setHistoryPage(0);
      setHistoryError("");
      setHistoryPhase("ready");
    } catch (reason: unknown) {
      if (requestId !== historyRequestRef.current) return;
      setHistoryError(String(reason));
      setHistoryPhase("error");
    }
  }

  useEffect(() => {
    if (!enabledModules.some((module) => module.id === moduleId)) setModuleId(preferredModuleId);
  }, [enabledModules, moduleId, preferredModuleId]);

  useEffect(() => {
    const configured = moduleConcurrency?.[moduleId] ?? defaultConcurrency;
    setConcurrency(Math.max(1, Math.min(32, configured)));
  }, [defaultConcurrency, moduleConcurrency, moduleId]);

  useEffect(() => {
    setDelayMs(defaultDelayMs);
  }, [defaultDelayMs]);

  useEffect(() => {
    if (proxiesLive === 0) setUseProxy(false);
  }, [proxiesLive]);

  useEffect(() => {
    if (!active || taskView !== "create" || !hasTauriRuntime()) {
      setDraggingSources(false);
      return;
    }

    let mounted = true;
    let cleanup: (() => void) | null = null;
    void getCurrentWindow().onDragDropEvent(({ payload }) => {
      if (!mounted) return;
      if (payload.type === "enter" || payload.type === "over") {
        setDraggingSources(true);
        return;
      }
      setDraggingSources(false);
      if (payload.type !== "drop") return;
      const paths = payload.paths.map((path) => path.trim()).filter(Boolean);
      if (paths.length === 0) return;
      setRawEntries((current) => [...new Set([
        ...current.split(/\r?\n/).map((entry) => entry.trim()).filter(Boolean),
        ...paths,
      ])].join("\n"));
      setMessage(`${paths.length.toLocaleString("en-US")} dropped ${paths.length === 1 ? "path" : "paths"} added.`);
    }).then((unlisten) => {
      if (mounted) cleanup = unlisten;
      else unlisten();
    }).catch(() => undefined);

    return () => {
      mounted = false;
      setDraggingSources(false);
      cleanup?.();
    };
  }, [active, taskView]);

  useEffect(() => {
    let mounted = true;
    const cleanups: Array<() => void> = [];
    const receive = (snapshot: TaskSnapshot) => {
      if (!mounted) return;
      acceptSnapshot(snapshot);
    };

    const initialize = async () => {
      const registrations = await Promise.allSettled([
        listen<TaskSnapshot>("task:progress", ({ payload }) => receive(payload)),
        listen<TaskSnapshot>("task:done", ({ payload }) => {
          if (!mounted) return;
          receive(payload);
          setCancelling(false);
          if (payload.discoveryError) {
            setMessage(payload.discoveryError);
          } else if (payload.historyPersisted === false) {
            setMessage("The task finished, but its summary could not be saved.");
          } else if (payload.resultsExportEnabled) {
            const exported = (payload.exportedActive ?? 0) + (payload.exportedFailed ?? 0);
            const exportErrors = payload.exportErrors ?? 0;
            setMessage(exportErrors > 0
              ? `${exported.toLocaleString("en-US")} result file(s) copied · ${exportErrors.toLocaleString("en-US")} could not be copied.`
              : `${exported.toLocaleString("en-US")} result file(s) copied to the selected folder.`);
          }
          void reloadHistory();
        }),
      ]);

      for (const registration of registrations) {
        if (registration.status !== "fulfilled") continue;
        if (mounted) cleanups.push(registration.value);
        else registration.value();
      }
      if (!mounted) return;

      const tasks = await invoke<TaskSnapshot[]>("list_tasks").catch(() => []);
      if (!mounted) return;
      const current = tasks.find((task) => taskIsRunning(task)) ?? tasks[0];
      if (current) {
        const detail = await invoke<TaskSnapshot | null>("get_task", { runId: current.runId }).catch(() => null);
        if (mounted) receive(detail ?? current);
      }
      if (mounted) void reloadHistory();
    };

    void initialize();

    return () => {
      mounted = false;
      cleanups.splice(0).forEach((cleanup) => cleanup());
    };
  }, []);

  async function start() {
    const entries = [...new Set(rawEntries.split(/\r?\n/).map((entry) => entry.trim()).filter(Boolean))];
    if (!moduleId) {
      setMessage("Select a module.");
      return;
    }
    if (entries.length === 0) {
      setMessage("Add at least one file or folder path.");
      return;
    }
    if (entryLimitExceeded) {
      setMessage("Use at most 10,000 file or folder paths.");
      return;
    }

    const safeConcurrency = Math.max(1, Math.min(32, Math.trunc(concurrency || 1)));
    const safeDelayMs = Math.max(0, Math.min(60_000, Math.trunc(delayMs || 0)));
    setStarting(true);
    setMessage("");
    try {
      const snapshot = await invoke<TaskSnapshot>("start_task", {
        request: { moduleId, entries, concurrency: safeConcurrency, delayMs: safeDelayMs, useProxy, outputDirectory: outputDirectory || null },
      });
      acceptSnapshot(snapshot);
      setRawEntries("");
      setConcurrency(safeConcurrency);
      setDelayMs(safeDelayMs);
      const duplicates = entryCount - entries.length;
      const preparationNotes = [
        snapshot.discoveryComplete === false
          ? "Streaming discovery started"
          : `${snapshot.total.toLocaleString("en-US")} structurally usable authentication ${snapshot.total === 1 ? "file" : "files"} found`,
        snapshot.discoveryComplete !== false && snapshot.locallyFiltered > 0 ? `${snapshot.locallyFiltered.toLocaleString("en-US")} unrelated or unusable ${snapshot.locallyFiltered === 1 ? "file" : "files"} ignored locally` : "",
        duplicates > 0 ? `${duplicates} duplicate source ${duplicates === 1 ? "path" : "paths"} removed` : "",
        outputDirectory ? `Results will be copied to ${outputFolderName}` : "",
      ].filter(Boolean);
      setMessage(`${preparationNotes.join(" · ")}. ${snapshot.discoveryComplete === false ? "Files will be validated as they are found." : "Authenticated validation started."}`);
      setTaskView("history");
      setHistoryPage(0);
    } catch (error) {
      setMessage(typeof error === "string" ? error : "The validation could not be started.");
    } finally {
      setStarting(false);
    }
  }

  async function cancel() {
    if (!activeTask || !activeRunning) return;
    setCancelling(true);
    setMessage("");
    try {
      const result = await invoke<TaskSnapshot | boolean>("cancel_task", { runId: activeTask.runId });
      if (typeof result === "object" && result) acceptSnapshot(result);
      setMessage("Cancellation requested. Current workers will stop safely.");
    } catch {
      setMessage("The task could not be cancelled.");
      setCancelling(false);
    }
  }

  async function clearHistory() {
    if (!window.confirm(`Clear all ${history.length} recorded ${history.length === 1 ? "task" : "tasks"}? This removes only summaries, not source or exported files.`)) return;
    try {
      await invoke("clear_task_history");
      historyRequestRef.current += 1;
      setHistory([]);
      setHistoryPage(0);
      setHistoryError("");
      setHistoryPhase("ready");
      onHistoryChanged();
    } catch {
      setMessage("The task history could not be cleared.");
    }
  }

  return (
    <section className="tasks-page" hidden={!active}>
      {taskView === "history" ? (
        <div className="task-history-view">
          <header className="task-page-header">
            <div><h1>Tasks</h1><p>Authenticated validation runs and recent results.</p></div>
            <div className="task-page-actions">
              <button className="button outline" type="button" onClick={clearHistory} disabled={history.length === 0 || activeRunning}><Trash2 size={14} /> Clear</button>
              <button className="button primary" type="button" onClick={() => { setMessage(""); setTaskView("create"); }} disabled={activeRunning || enabledModules.length === 0}><Plus size={14} /> Create</button>
            </div>
          </header>

          {activeTask && activeRunning && (
            <article className="task-active-panel">
              <div className="task-active-identity">
                <span className="task-active-icon"><ModuleBrandIcon id={activeTask.moduleId} /></span>
                <div><strong>{moduleById.get(activeTask.moduleId)?.name ?? activeTask.moduleId}</strong><small>{planSummary || `Run ${activeTask.runId.slice(0, 8)}`}</small></div>
                <span className={`task-status ${taskTone(activeTask.status)}`}>{taskStatusLabel(activeTask.status)}</span>
              </div>
              <div className="task-active-progress">
                <div><span>{activeDiscovering ? `${activeTask.discovered.toLocaleString("en-US")} files scanned` : `${Math.max(0, activeTask.total - activeTask.queued - activeTask.running)} of ${activeTask.total}`}</span><strong>{activeDiscovering ? "Discovering…" : `${Math.round(progress)}%`}</strong></div>
                {activeDiscovering ? <progress className="task-progress discovering" aria-label="Discovering task files" /> : <progress className="task-progress" value={progress} max="100" />}
              </div>
              <div className="task-active-stats">
                {activeDiscovering && <span>{activeTask.discovered} discovered</span>}<span>{activeTask.queued} queued</span><span>{activeTask.running} running</span><span>{outcomeSummary?.active ?? activeTask.succeeded} active</span>{moduleSummary && !chatgptSummary && (moduleSummary.authenticatedUnknown ?? 0) > 0 && <span>{moduleSummary.authenticatedUnknown ?? 0} authenticated · plan unknown</span>}{chatgptSummary && (chatgptSummary.authenticatedUnknown ?? 0) > 0 && <span>{chatgptSummary.authenticatedUnknown ?? 0} authenticated · plan unknown</span>}{chatgptSummary && (chatgptSummary.planUnavailable ?? 0) > 0 && <span>{chatgptSummary.planUnavailable ?? 0} authenticated · plan unavailable</span>}{moduleSummary && (moduleSummary.noEntitlement ?? 0) > 0 && <span>{moduleSummary.noEntitlement ?? 0} no entitlement</span>}<span>{outcomeSummary?.dead ?? activeTask.failed} {hasDetailedOutcome ? "dead" : "failed"}</span>{moduleSummary && moduleSummary.rateLimited > 0 && <span>{moduleSummary.rateLimited} rate limited</span>}{moduleSummary && moduleSummary.errors > 0 && <span>{moduleSummary.errors} errors</span>}{moduleSummary && moduleSummary.invalid > 0 && <span>{moduleSummary.invalid} invalid</span>}{activeTask.locallyFiltered > 0 && <span>{activeTask.locallyFiltered} ignored locally</span>}<span>{activeTask.skipped} skipped</span><span>{activeTask.retried} retries</span><span>{activeTask.useProxy ? `${activeTask.proxyCount} proxy pool` : "Direct"}</span>{activeTask.resultsExportEnabled && <><span>{activeTask.exportedActive ?? 0} active copied</span><span>{activeTask.exportedFailed ?? 0} failed copied</span>{(activeTask.exportErrors ?? 0) > 0 && <span className="task-export-error">{activeTask.exportErrors} copy errors</span>}</>}
              </div>
              <button className="button danger task-active-cancel" type="button" onClick={cancel} disabled={cancelling}><StopCircle size={14} />{cancelling ? "Cancelling…" : "Cancel"}</button>
            </article>
          )}

          {message && <p className="form-message task-page-message">{message}</p>}

          <section className="task-history-panel" aria-labelledby="task-history-title">
            <header className="task-history-header"><div><h2 id="task-history-title">History</h2><p>{history.length} recorded {history.length === 1 ? "task" : "tasks"}</p></div></header>
            {historyPhase === "loading" ? (
              <div className="empty-state task-history-empty" role="status"><LoaderCircle className="is-spinning" size={21} /><strong>Loading task history</strong><span>Reading saved summaries.</span></div>
            ) : historyPhase === "error" ? (
              <div className="empty-state task-history-empty"><CircleDot size={21} /><strong>Task history could not be loaded</strong><span title={historyError}>Existing summaries were not replaced with an empty state.</span><button className="button outline small" type="button" onClick={() => void reloadHistory()}>Retry</button></div>
            ) : history.length === 0 ? (
              <div className="empty-state task-history-empty"><Clock3 size={21} /><strong>No tasks yet</strong><span>Create a task to begin.</span></div>
            ) : (
              <div className="task-history-list">
                {visibleHistory.map((task) => (
                  <article className="task-history-row" key={task.runId}>
                    <span className="task-history-module"><ModuleBrandIcon id={task.moduleId} /></span>
                    <div className="task-history-copy"><strong>{moduleById.get(task.moduleId)?.name ?? task.moduleId}</strong><small className={task.discoveryError ? "task-history-error" : undefined} title={task.discoveryError ?? undefined}>{task.discoveryError ? `Discovery failed: ${task.discoveryError}` : `${formatTaskDate(task.finishedAt ?? task.startedAt)} · ${task.runId.slice(0, 8)} · ${task.useProxy ? `${task.proxyCount ?? 0} proxies` : "Direct"} · ${task.concurrency} workers`}</small></div>
                    <div className="task-history-metrics"><span>{task.total} candidates</span><span>{task.succeeded} active</span><span>{task.failed} failed</span>{(task.locallyFiltered ?? 0) > 0 && <span>{task.locallyFiltered} ignored locally</span>}<span>{task.skipped} skipped</span>{task.resultsExportEnabled && <span>{(task.exportedActive ?? 0) + (task.exportedFailed ?? 0)} copied{(task.exportErrors ?? 0) > 0 ? ` · ${task.exportErrors} errors` : ""}</span>}</div>
                    <span className={`task-status ${taskTone(task.status)}`}>{taskStatusLabel(task.status)}</span>
                  </article>
                ))}
              </div>
            )}
            {historyPageCount > 1 && <footer className="task-history-pagination"><button className="button outline small" type="button" disabled={historyPage === 0} onClick={() => setHistoryPage((page) => Math.max(0, page - 1))}>Previous</button><span>{historyPage + 1} of {historyPageCount}</span><button className="button outline small" type="button" disabled={historyPage + 1 >= historyPageCount} onClick={() => setHistoryPage((page) => Math.min(historyPageCount - 1, page + 1))}>Next</button></footer>}
          </section>
        </div>
      ) : (
        <div className="task-create-view">
          <header className="task-create-header">
            <button className="task-back-button" type="button" onClick={() => { setMessage(""); setTaskView("history"); }}><ArrowLeft size={15} /> Back to tasks</button>
            <h1>Create task</h1>
            <p>Choose what to validate, add authorized sources, then tune how the run should behave.</p>
          </header>

          <div className="task-create-sections">
            <section className="settings-section">
              <h2><span className="section-step">1</span>Task setup</h2>
              <div className="settings-group task-create-group">
                <div className="settings-row task-module-row">
                  <div className="settings-copy"><strong>Module</strong><small>Only modules enabled in the catalog appear here.</small></div>
                  <div className="task-module-picker" role="group" aria-label="Enabled modules">
                    {enabledModules.map((module) => <button className={moduleId === module.id ? "task-module-choice active" : "task-module-choice"} type="button" aria-pressed={moduleId === module.id} key={module.id} onClick={() => setModuleId(module.id)} disabled={starting}><span><ModuleBrandIcon id={module.id} /></span>{module.name}</button>)}
                  </div>
                </div>
                <div className={`settings-row task-path-row${draggingSources ? " drag-active" : ""}`}>
                  <div className="settings-copy"><label htmlFor="task-entries">Authorized files or folders</label><small>{draggingSources ? "Drop the files or folders to add their local paths." : "Paste one local path per line, drag sources here, or use the pickers. Duplicates are removed automatically."}</small></div>
                  <div className="task-source-pickers" aria-label="Add authorized sources">
                    <button className="button outline small" type="button" onClick={() => void chooseSources("files")} disabled={starting || selectingSources}><FileUp size={13} />Choose files</button>
                    <button className="button outline small" type="button" onClick={() => void chooseSources("folder")} disabled={starting || selectingSources}><FolderOpen size={13} />Choose folder</button>
                  </div>
                  <textarea className={`field-input task-entry-input${entryLimitExceeded ? " invalid" : ""}`} id="task-entries" value={rawEntries} onChange={(event) => setRawEntries(event.target.value)} placeholder={"C:\\Ayla\\examples"} disabled={starting} spellCheck={false} aria-invalid={entryLimitExceeded} />
                </div>
                <div className="settings-row task-source-row"><div className="settings-copy"><strong>Source preview</strong><small>{entryLimitExceeded ? "The 10,000-path limit has been exceeded." : duplicateEntryCount ? `${duplicateEntryCount} duplicate path(s) will be removed.` : "Cookie totals are calculated after the local scan starts."}</small></div><div className="task-source-summary"><span><strong>{uniqueEntryCount.toLocaleString("en-US")}</strong>Paths</span><span><strong>—</strong>Cookies</span></div></div>
                <div className="settings-row task-output-row">
                  <div className="settings-copy"><strong>Results folder <span className="optional-label">Optional</span></strong><small>{`Copies results into ${selectedModule?.id ?? "module"}/active and ${selectedModule?.id ?? "module"}/failed. Original files stay untouched.`}</small></div>
                  <div className="task-output-control">
                    <button className="button outline small task-output-picker" type="button" onClick={() => void chooseOutputDirectory()} disabled={starting || selectingOutputDirectory} title={outputDirectory || undefined}><FolderOpen size={13} /><span>{selectingOutputDirectory ? "Opening…" : outputFolderName || "Choose folder"}</span></button>
                    {outputDirectory && <button className="icon-button ghost task-output-clear" type="button" onClick={() => { setOutputDirectory(""); setMessage(""); }} disabled={starting || selectingOutputDirectory} aria-label="Clear results folder" title="Clear results folder"><X size={14} /></button>}
                  </div>
                </div>
              </div>
            </section>

            <section className="settings-section">
              <h2><span className="section-step">2</span>Run options</h2>
              <div className="settings-group task-create-group">
                <div className="settings-row task-proxy-setting"><div className="settings-copy"><strong>Proxy routing</strong><small>{proxiesLive > 0 ? `${proxiesLive} HTTPS-verified ${proxiesLive === 1 ? "proxy" : "proxies"} available` : "Add and check proxies before enabling this option."}</small>{proxiesLive === 0 && <button className="inline-action" type="button" onClick={onOpenProxies}>Manage proxies</button>}</div><button className={useProxy ? "switch-control active" : "switch-control"} type="button" role="switch" aria-checked={useProxy} aria-label="Route this task through HTTPS-verified proxies" onClick={() => setUseProxy((current) => !current)} disabled={starting || proxiesLive === 0}><span /></button></div>
                <SettingNumberRow id="task-concurrency" label="Concurrency" description={useProxy && effectiveConcurrency < requestedConcurrency ? `${requestedConcurrency} requested · ${effectiveConcurrency} effective with the current proxy pool.` : "Parallel workers. Higher values use more CPU and network capacity."} min={1} max={32} value={concurrency} onChange={setConcurrency} disabled={starting} />
                <SettingNumberRow id="task-delay" label={selectedModule && selectedModule.id !== "chatgpt" ? "Request spacing" : "Worker delay"} description={selectedModule && selectedModule.id !== "chatgpt" ? `Minimum global spacing between ${selectedModule.name} requests, including retries and proxy failover.` : "Pause between entries per worker, in milliseconds."} min={0} max={60_000} value={delayMs} onChange={setDelayMs} disabled={starting} />
              </div>
            </section>
          </div>

          <footer className="task-create-footer"><div className="task-create-summary" aria-live="polite">{selectedModule && <span className="task-create-summary-icon"><ModuleBrandIcon id={selectedModule.id} /></span>}<span><strong>{message || selectedModule?.name || "Select a module"}</strong><small>{entryCount ? `${uniqueEntryCount.toLocaleString("en-US")} unique path(s) · ${useProxy ? `${effectiveConcurrency} workers across ${proxiesLive} live proxies` : `${effectiveConcurrency} workers · Direct connection`}${outputDirectory ? ` · Save to ${outputFolderName}` : ""}` : "Add at least one authorized path"}</small></span></div><button className="button outline" type="button" onClick={() => setTaskView("history")} disabled={starting}>Cancel</button><button className="button primary" type="button" onClick={start} disabled={!moduleId || starting || entryCount === 0 || entryLimitExceeded}><Play size={14} />{starting ? "Preparing…" : "Start task"}</button></footer>
        </div>
      )}
    </section>
  );
}
function countryFlag(countryCode: string) {
  const code = countryCode.trim().toUpperCase();
  if (!/^[A-Z]{2}$/.test(code)) return "🌐";
  return String.fromCodePoint(...[...code].map((character) => 127397 + character.charCodeAt(0)));
}

function Proxies({ active, defaultThreads, defaultTimeoutMs, onCountsChanged }: { active: boolean; defaultThreads: number; defaultTimeoutMs: number; onCountsChanged: (total: number, live: number) => void }) {
  const [raw, setRaw] = useState("");
  const [protocol, setProtocol] = useState("http");
  const [items, setItems] = useState<ProxyItem[]>([]);
  const itemsRef = useRef<ProxyItem[]>([]);
  const [report, setReport] = useState<AddProxiesResult | null>(null);
  const [parseReport, setParseReport] = useState<ProxyParseReport | null>(null);
  const [progress, setProgress] = useState<ProxyProgress | null>(null);
  const [selectedIds, setSelectedIds] = useState<string[]>([]);
  const [selectedProxyId, setSelectedProxyId] = useState<string | null>(null);
  const [proxyView, setProxyView] = useState<"list" | "add">("list");
  const [proxyFilter, setProxyFilter] = useState<"all" | "live" | "pending" | "httpOnly" | "failed">("all");
  const [query, setQuery] = useState("");
  const [proxyPage, setProxyPage] = useState(0);
  const [proxyPageSize, setProxyPageSize] = useState(5);
  const [running, setRunning] = useState(false);
  const [message, setMessage] = useState("");
  const [loadPhase, setLoadPhase] = useState<ResourcePhase>("loading");
  const [loadError, setLoadError] = useState("");
  const proxyFileInputRef = useRef<HTMLInputElement>(null);
  const proxyListRef = useRef<HTMLDivElement>(null);
  const pendingProxyUpdatesRef = useRef<Map<string, ProxyItem>>(new Map());
  const latestProxyProgressRef = useRef<ProxyProgress | null>(null);
  const proxyFlushTimerRef = useRef(0);
  const acceptProxyEventsRef = useRef(false);
  const selectedIdSet = useMemo(() => new Set(selectedIds), [selectedIds]);

  function applyItems(nextItems: ProxyItem[]) {
    itemsRef.current = nextItems;
    const availableIds = new Set(nextItems.map((item) => item.id));
    setItems(nextItems);
    setSelectedIds((current) => current.filter((id) => availableIds.has(id)));
    setSelectedProxyId((current) => current && availableIds.has(current) ? current : nextItems[0]?.id ?? null);
    onCountsChanged(nextItems.length, nextItems.filter((item) => item.status === "live").length);
  }

  function applyAuthoritativeItems(nextItems: ProxyItem[]) {
    window.clearTimeout(proxyFlushTimerRef.current);
    proxyFlushTimerRef.current = 0;
    pendingProxyUpdatesRef.current.clear();
    latestProxyProgressRef.current = null;
    applyItems(nextItems);
  }

  async function reloadProxies() {
    setLoadPhase("loading");
    setLoadError("");
    try {
      applyAuthoritativeItems(await invoke<ProxyItem[]>("list_proxies"));
      setLoadPhase("ready");
    } catch (reason: unknown) {
      setLoadError(String(reason));
      setLoadPhase("error");
    }
  }

  useEffect(() => {
    let active = true;
    const cleanups: Array<() => void> = [];

    void reloadProxies();
    invoke<boolean>("is_proxy_check_running").then((value) => {
      if (!active) return;
      acceptProxyEventsRef.current = value;
      setRunning(value);
    }).catch(() => undefined);

    void listen<ProxyProgress>("proxy:progress", ({ payload }) => {
      if (!active || !acceptProxyEventsRef.current) return;
      latestProxyProgressRef.current = payload;
      if (payload.item) pendingProxyUpdatesRef.current.set(payload.item.id, payload.item);
      if (proxyFlushTimerRef.current) return;
      proxyFlushTimerRef.current = window.setTimeout(() => {
        proxyFlushTimerRef.current = 0;
        if (!active) return;
        const updates = pendingProxyUpdatesRef.current;
        if (updates.size > 0) {
          const knownIds = new Set(itemsRef.current.map((item) => item.id));
          const next = itemsRef.current.map((item) => updates.get(item.id) ?? item);
          updates.forEach((item, id) => { if (!knownIds.has(id)) next.push(item); });
          updates.clear();
          applyItems(next);
        }
        const latest = latestProxyProgressRef.current;
        if (latest) {
          setProgress(latest);
          setRunning(latest.running);
          if (!latest.running) acceptProxyEventsRef.current = false;
        }
      }, 75);
    }).then((unlisten) => active ? cleanups.push(unlisten) : unlisten()).catch(() => undefined);

    return () => {
      active = false;
      window.clearTimeout(proxyFlushTimerRef.current);
      proxyFlushTimerRef.current = 0;
      cleanups.forEach((cleanup) => cleanup());
    };
  }, []);

  useEffect(() => {
    let active = true;
    if (!raw.trim()) {
      setParseReport(null);
      return () => { active = false; };
    }
    setParseReport(null);
    const timer = window.setTimeout(() => {
      void invoke<ProxyParseReport>("parse_proxy_input", { raw, protocol })
        .then((nextReport) => { if (active) setParseReport(nextReport); })
        .catch(() => { if (active) setParseReport(null); });
    }, 160);
    return () => {
      active = false;
      window.clearTimeout(timer);
    };
  }, [protocol, raw]);

  async function add() {
    if (!raw.trim()) {
      setMessage("Paste at least one proxy to import.");
      return;
    }
    try {
      const sourceLines = raw.split(/\r?\n/);
      const result = await invoke<AddProxiesResult>("add_proxies", { raw, protocol });
      setReport(result);
      applyAuthoritativeItems(result.items);
      setRaw([...new Set(result.rejected.map((item) => sourceLines[item.line - 1]?.trim()).filter(Boolean))].join("\n"));
      setProxyPage(0);
      setMessage(`${result.added} added · ${result.duplicates} duplicate · ${result.rejected.length} rejected.`);
      if (result.added > 0 && result.duplicates === 0 && result.rejected.length === 0) setProxyView("list");
    } catch (reason: unknown) {
      setMessage(String(reason));
    }
  }

  async function importProxyFile(file: File | undefined) {
    if (!file) return;
    if (file.size > MAX_PROXY_IMPORT_BYTES) {
      setMessage("That file is too large. Choose a text file up to 16 MiB.");
      return;
    }

    try {
      const contents = await file.text();
      if (contents.includes("\0")) {
        setMessage("This does not appear to be a plain-text proxy file.");
        return;
      }

      const normalized = contents.replace(/\r\n?/g, "\n").trim();
      if (!normalized) {
        setMessage("The selected file is empty.");
        return;
      }

      const importedLines = normalized.split("\n").filter((line) => line.trim()).length;
      setRaw((current) => [current.trimEnd(), normalized].filter(Boolean).join("\n"));
      setReport(null);
      setMessage(`${importedLines.toLocaleString("en-US")} lines loaded from ${file.name}. Review the preview before adding them.`);
    } catch {
      setMessage("Ayla could not read that file. Check its permissions and try again.");
    }
  }

  async function check(ids: string[]) {
    if (items.length === 0) return;
    const total = ids.length || items.length;
    acceptProxyEventsRef.current = true;
    setRunning(true);
    setMessage("");
    setProgress({ done: 0, total, percent: 0, live: 0, httpOnly: 0, failed: 0, removed: 0, id: "", item: null, status: "running", running: true });
    try {
      const result = await invoke<{ results: ProxyItem[]; stopped: boolean; done: number; total: number; live: number; httpOnly: number; failed: number }>("check_proxies", { request: { ids, threads: defaultThreads, timeoutMs: defaultTimeoutMs } });
      acceptProxyEventsRef.current = false;
      applyAuthoritativeItems(result.results);
      setProgress({
        done: result.done,
        total: result.total,
        percent: result.total > 0 ? Math.min(100, (result.done / result.total) * 100) : 0,
        live: result.live,
        httpOnly: result.httpOnly,
        failed: result.failed,
        removed: 0,
        id: "",
        item: null,
        status: result.stopped ? "stopped" : "done",
        running: false,
      });
      setMessage(result.stopped
        ? "Check stopped. Saved proxies were retained."
        : `Check completed: ${result.live} HTTPS-ready · ${result.httpOnly} HTTP-only · ${result.failed} failed. No proxies were removed.`);
    } catch (reason: unknown) {
      acceptProxyEventsRef.current = false;
      window.clearTimeout(proxyFlushTimerRef.current);
      proxyFlushTimerRef.current = 0;
      pendingProxyUpdatesRef.current.clear();
      latestProxyProgressRef.current = null;
      await reloadProxies();
      setProgress(null);
      setMessage(String(reason));
    } finally {
      setRunning(false);
    }
  }

  async function stop() {
    try {
      const stopped = await invoke<boolean>("stop_proxy_check");
      if (stopped) setMessage("Stop requested. Current connections will finish at timeout.");
    } catch (reason: unknown) {
      setMessage(String(reason));
    }
  }

  async function remove(id: string) {
    const target = items.find((item) => item.id === id);
    if (!window.confirm(`Remove ${target?.display ?? "this proxy"} from the saved list? This cannot be undone.`)) return;
    try {
      applyAuthoritativeItems(await invoke<ProxyItem[]>("remove_proxies", { ids: [id] }));
    } catch (reason: unknown) {
      setMessage(String(reason));
    }
  }

  async function clear() {
    if (!window.confirm(`Remove all ${items.length} saved ${items.length === 1 ? "proxy" : "proxies"}? This cannot be undone.`)) return;
    try {
      applyAuthoritativeItems(await invoke<ProxyItem[]>("clear_proxies"));
      setReport(null);
      setProgress(null);
      setSelectedIds([]);
      setSelectedProxyId(null);
      setProxyPage(0);
      setRunning(false);
      setMessage("List cleared.");
    } catch (reason: unknown) {
      setMessage(String(reason));
    }
  }

  async function removeFailed() {
    const ids = items.filter((item) => item.status === "failed").map((item) => item.id);
    if (ids.length === 0) return;
    if (!window.confirm(`Remove ${ids.length} failed ${ids.length === 1 ? "proxy" : "proxies"}? HTTP-only proxies will be kept for diagnostics.`)) return;
    try {
      applyAuthoritativeItems(await invoke<ProxyItem[]>("remove_proxies", { ids }));
      setMessage(`${ids.length} failed ${ids.length === 1 ? "proxy" : "proxies"} removed.`);
    } catch (reason: unknown) {
      acceptProxyEventsRef.current = false;
      setMessage(String(reason));
    }
  }

  const liveCount = items.filter((item) => item.status === "live").length;
  const httpOnlyCount = items.filter((item) => item.status === "httpOnly").length;
  const failedCount = items.filter((item) => item.status === "failed").length;
  const proxyInputLines = useMemo(() => raw.split(/\r?\n/).map((line) => line.trim()).filter(Boolean), [raw]);
  const proxyInputCandidates = useMemo(() => raw.split(/[\s,;]+/).map((entry) => entry.trim()).filter(Boolean), [raw]);
  const proxyUniqueCandidates = useMemo(() => new Set(proxyInputCandidates).size, [proxyInputCandidates]);
  const proxyCredentialCandidates = useMemo(() => proxyInputCandidates.filter((entry) => entry.includes("@")).length, [proxyInputCandidates]);
  const parsedAccepted = parseReport?.accepted.length ?? proxyUniqueCandidates;
  const parsedRejected = parseReport?.rejected.length ?? 0;
  const parsedDuplicates = parseReport?.duplicates ?? Math.max(0, proxyInputCandidates.length - proxyUniqueCandidates);
  const parsedWithAuth = parseReport?.accepted.filter((item) => item.hasAuth).length ?? proxyCredentialCandidates;
  const filteredItems = useMemo(() => {
    const normalizedQuery = query.trim().toLowerCase();
    return items.filter((item) => {
      const matchesFilter = proxyFilter === "all" || item.status === proxyFilter;
      const searchable = `${item.display} ${item.host} ${item.ip} ${item.country} ${item.countryCode} ${item.city} ${item.protocol}`.toLowerCase();
      return matchesFilter && (!normalizedQuery || searchable.includes(normalizedQuery));
    });
  }, [items, proxyFilter, query]);
  useEffect(() => {
    const list = proxyListRef.current;
    if (!list || proxyView !== "list" || filteredItems.length === 0 || typeof ResizeObserver === "undefined") return;

    const updatePageSize = () => {
      const nextPageSize = Math.max(1, Math.floor(list.clientHeight / 64));
      setProxyPageSize((current) => current === nextPageSize ? current : nextPageSize);
    };
    updatePageSize();
    const observer = new ResizeObserver(updatePageSize);
    observer.observe(list);
    return () => observer.disconnect();
  }, [filteredItems.length, proxyView]);

  const proxyPageCount = Math.max(1, Math.ceil(filteredItems.length / proxyPageSize));
  const visibleProxyItems = filteredItems.slice(proxyPage * proxyPageSize, (proxyPage + 1) * proxyPageSize);
  const selectedProxy = items.find((item) => item.id === selectedProxyId) ?? null;

  useEffect(() => {
    setProxyPage((current) => Math.min(current, proxyPageCount - 1));
  }, [proxyPageCount]);

  function toggleSelected(id: string) {
    setSelectedIds((current) => {
      const next = new Set(current);
      next.has(id) ? next.delete(id) : next.add(id);
      return [...next];
    });
  }

  return (
    <section className="proxies-page" hidden={!active}>
      {proxyView === "list" ? (
        <div className="proxy-browser-view">
          <section className="proxy-list-pane" aria-label="Saved proxies">
            <div className="proxy-list-tabs" aria-label="Filter proxies by status">
              {(["all", "live", "httpOnly", "failed", "pending"] as const).map((status) => (
                <button className={proxyFilter === status ? "active" : ""} type="button" aria-pressed={proxyFilter === status} key={status} onClick={() => { setProxyFilter(status); setProxyPage(0); }}>{status === "all" ? "All" : status === "live" ? "HTTPS ready" : status === "httpOnly" ? "HTTP only" : status === "failed" ? "Failed" : "Unchecked"}</button>
              ))}
            </div>

            <div className="proxy-search-shell">
              <Search size={15} />
              <input type="search" value={query} onChange={(event) => { setQuery(event.target.value); setProxyPage(0); }} placeholder="Search proxies" aria-label="Search proxies" />
              <button className="proxy-add-pill" type="button" onClick={() => { setMessage(""); setProxyView("add"); }} disabled={running}><Plus size={13} />Add proxy</button>
            </div>

            <div className="proxy-list-summary">
              <span>{filteredItems.length} shown · {liveCount} HTTPS ready · {httpOnlyCount} HTTP only · {failedCount} failed</span>
              <div className="proxy-list-summary-actions">
                {running ? <button className="proxy-list-run" onClick={stop} type="button"><StopCircle size={11} />Stop</button> : <button className="proxy-list-run" onClick={() => check(selectedIds)} type="button" disabled={items.length === 0} title={selectedIds.length ? `Check ${selectedIds.length} selected proxies` : "Check every saved proxy"}><Play size={11} />Run checks</button>}
                <button type="button" onClick={removeFailed} disabled={running || failedCount === 0} title="Permanently remove only proxies whose latest check failed">Remove failed</button>
                <button type="button" onClick={() => setSelectedIds(selectedIds.length === items.length ? [] : items.map((item) => item.id))} disabled={running || items.length === 0}>{selectedIds.length === items.length && items.length > 0 ? "Deselect all" : "Select all"}</button>
              </div>
            </div>

            {progress && (
              <div className="proxy-check-progress proxy-check-progress-compact">
                <div><span>{progress.done} of {progress.total}</span><strong>{Math.round(progress.percent)}%</strong></div>
                <progress value={progress.percent} max="100" />
                <small>{progress.live} HTTPS ready · {progress.httpOnly} HTTP only · {progress.failed} failed · none removed</small>
              </div>
            )}

            {loadPhase === "loading" ? (
              <div className="proxy-list-empty" role="status"><LoaderCircle className="is-spinning" size={22} /><strong>Loading saved proxies</strong><span>The list is being read from local storage.</span></div>
            ) : loadPhase === "error" ? (
              <div className="proxy-list-empty"><CircleDot size={22} /><strong>Saved proxies could not be loaded</strong><span title={loadError}>The existing file was not treated as an empty list.</span><button className="button outline small" type="button" onClick={() => void reloadProxies()}>Retry</button></div>
            ) : filteredItems.length === 0 ? (
              <div className="proxy-list-empty"><Network size={22} /><strong>{items.length ? "No matching proxies" : "No proxies yet"}</strong><span>{items.length ? "Try another search or filter." : "Add a proxy list to begin."}</span></div>
            ) : (
              <div className="proxy-browser-list" ref={proxyListRef}>
                {visibleProxyItems.map((item) => {
                  const location = [item.city, item.country].filter(Boolean).join(", ") || "Location unavailable";
                  return (
                    <div className={`proxy-list-row${selectedProxy?.id === item.id ? " active" : ""}`} key={item.id}>
                      <input className="proxy-check" type="checkbox" checked={selectedIdSet.has(item.id)} onChange={() => toggleSelected(item.id)} disabled={running} aria-label={`Select ${item.display}`} />
                      <button className="proxy-row-select" type="button" onClick={() => setSelectedProxyId(item.id)}>
                        <span className="proxy-country-flag" aria-label={item.country || "Unknown country"}>{countryFlag(item.countryCode)}</span>
                        <span className="proxy-row-copy"><strong>{item.ip || item.host}</strong><small>{location} · {item.hasAuth ? "Authenticated" : "No authentication"}</small></span>
                        <span className="proxy-row-meta"><strong>{item.checkedAt ? `${item.latencyMs} ms` : "—"}</strong><small>{item.status === "live" ? "HTTPS" : item.status === "httpOnly" ? "HTTP only" : item.status === "failed" ? "Failed" : item.protocol.toUpperCase()}</small></span>
                      </button>
                    </div>
                  );
                })}
              </div>
            )}
            {proxyPageCount > 1 && (
              <footer className="proxy-list-pagination">
                <button type="button" aria-label="Previous proxy page" onClick={() => setProxyPage((current) => current - 1)} disabled={proxyPage === 0}><ArrowLeft size={12} /></button>
                <span>{proxyPage + 1} of {proxyPageCount}</span>
                <button type="button" aria-label="Next proxy page" onClick={() => setProxyPage((current) => current + 1)} disabled={proxyPage + 1 >= proxyPageCount}><ArrowRight size={12} /></button>
              </footer>
            )}
          </section>

          <aside className="proxy-detail-pane">
            <header className="proxy-detail-toolbar">
              <div><strong>Proxy details</strong><span>{items.length} stored · {defaultThreads} workers · {Math.round(defaultTimeoutMs / 1_000)}s timeout</span></div>
              <div>
                <button className="button outline small" onClick={removeFailed} type="button" disabled={failedCount === 0 || running}><Trash2 size={13} />Remove failed</button>
                <button className="button danger small" onClick={clear} type="button" disabled={items.length === 0 || running}><Trash2 size={13} />Clear all</button>
              </div>
            </header>

            {progress && (
              <div className="proxy-check-progress">
                <div><span>{progress.done} of {progress.total}</span><strong>{Math.round(progress.percent)}%</strong></div>
                <progress value={progress.percent} max="100" />
                <small>{progress.live} HTTPS ready · {progress.httpOnly} HTTP only · {progress.failed} failed · none removed</small>
              </div>
            )}

            {message && <p className="proxy-detail-message">{message}</p>}

            {selectedProxy ? (
              <div className="proxy-detail-content">
                <div className="proxy-detail-hero">
                  <span className="proxy-detail-flag">{countryFlag(selectedProxy.countryCode)}</span>
                  <div><h1>{selectedProxy.ip || selectedProxy.host}</h1><p>{[selectedProxy.city, selectedProxy.country].filter(Boolean).join(", ") || "Location unavailable"}</p></div>
                  <span className={`proxy-detail-status ${selectedProxy.status}`}>{selectedProxy.status === "live" ? "HTTPS ready" : selectedProxy.status === "httpOnly" ? "HTTP only" : selectedProxy.status === "failed" ? "Failed" : "Unchecked"}</span>
                </div>

                <div className="proxy-detail-grid">
                  <div><span>Address</span><strong>{selectedProxy.host}:{selectedProxy.port}</strong></div>
                  <div><span>Protocol</span><strong>{selectedProxy.protocol.toUpperCase()}</strong></div>
                  <div><span>HTTPS capability</span><strong>{selectedProxy.capability === "httpsVerified" ? "Verified" : selectedProxy.capability === "httpOnly" ? "Unavailable; HTTP only" : selectedProxy.capability === "unavailable" ? "Latest check failed" : "Not checked"}</strong></div>
                  <div><span>Latency</span><strong>{selectedProxy.checkedAt ? `${selectedProxy.latencyMs} ms` : "Not checked"}</strong></div>
                  <div><span>Authentication</span><strong>{selectedProxy.hasAuth ? "Credentials saved" : "None"}</strong></div>
                  <div><span>Last checked</span><strong>{selectedProxy.checkedAt ? formatTaskDate(selectedProxy.checkedAt) : "Never"}</strong></div>
                  <div><span>Result</span><strong>{selectedProxy.message || "Waiting for a check"}</strong></div>
                </div>

                <div className="proxy-detail-actions">
                  <button className="button outline" type="button" onClick={() => check([selectedProxy.id])} disabled={running}><Play size={13} />Check this proxy</button>
                  <button className="button danger" type="button" onClick={() => remove(selectedProxy.id)} disabled={running}><Trash2 size={13} />Remove</button>
                </div>
              </div>
            ) : (
              <div className="proxy-detail-empty"><Network size={24} /><strong>Select a proxy to view</strong><span>Latency, location, protocol, and check results appear here.</span></div>
            )}
          </aside>
        </div>
      ) : (
        <div className="proxy-add-view">
          <header className="proxy-add-header">
            <button className="task-back-button" type="button" onClick={() => { setMessage(""); setProxyView("list"); }}><ArrowLeft size={15} />Back to proxies</button>
            <h1>Add proxy</h1>
            <p>Paste a list, confirm the default protocol, and review the import before saving locally.</p>
          </header>

          <div className="proxy-add-sections">
            <section className="settings-section">
              <h2><span className="section-step">1</span>Proxy list</h2>
              <div className="settings-group proxy-add-group">
                <div className="settings-row proxy-list-input-row">
                  <div className="proxy-input-heading">
                    <div className="settings-copy"><label htmlFor="proxy-input">Proxy addresses</label><small>Paste entries or load a text file. Existing text is preserved, and rejected source lines stay here for correction.</small></div>
                    <button className="button outline small proxy-file-button" type="button" onClick={() => proxyFileInputRef.current?.click()} disabled={running}><FileUp size={13} />Import from file</button>
                    <input ref={proxyFileInputRef} type="file" hidden onChange={(event) => { const file = event.currentTarget.files?.[0]; event.currentTarget.value = ""; void importProxyFile(file); }} disabled={running} />
                  </div>
                  <textarea className="field-input proxy-add-input" id="proxy-input" value={raw} onChange={(event) => { setRaw(event.target.value); setMessage(""); }} placeholder={"1.2.3.4:8080\nuser:password@host:3128\nhost:3128:user:password\nsocks5://host:1080"} disabled={running} spellCheck={false} />
                  <div className="proxy-input-meta"><span>{parsedAccepted} accepted</span><span>{parsedDuplicates} duplicate</span><span>{parsedRejected} rejected</span></div>
                </div>
                <div className="settings-row proxy-protocol-row">
                  <div className="settings-copy"><strong>Default protocol</strong><small>Only applied when a line has no protocol prefix.</small></div>
                  <div className="proxy-protocol-options" role="group" aria-label="Default proxy protocol">
                    {(["http", "socks4", "socks5"] as const).map((option) => <button className={protocol === option ? "active" : ""} type="button" aria-pressed={protocol === option} key={option} onClick={() => setProtocol(option)} disabled={running}>{option.toUpperCase()}</button>)}
                  </div>
                </div>
              </div>
            </section>

            <section className="settings-section proxy-import-report">
              <h2><span className="section-step">2</span>Import preview</h2>
              <div className="settings-group">
                <div className="proxy-import-stats"><div><strong>{parsedAccepted}</strong><span>Accepted</span></div><div><strong>{parsedDuplicates}</strong><span>Duplicates</span></div><div><strong>{parsedWithAuth}</strong><span>With auth</span></div></div>
                <div className="proxy-local-note"><ShieldCheck size={16} /><div><strong>Local by default</strong><span>Credentials are never displayed after import.</span></div></div>
                {parseReport?.rejected.slice(0, 3).map((item) => <p className="proxy-import-rejection" key={`preview-${item.line}-${item.reason}`}>Line {item.line}: {item.reason}</p>)}
                {parseReport && parseReport.rejected.length > 3 && <p className="proxy-import-rejection">{parseReport.rejected.length - 3} more rejected entries</p>}
                {report && <div className="proxy-last-import"><span>Last import</span><strong>{report.added} added · {report.duplicates} duplicate · {report.rejected.length} rejected</strong></div>}
              </div>
            </section>
          </div>

          <footer className="proxy-add-footer"><span role="status">{message || (proxyInputLines.length ? `${parsedAccepted} accepted proxies ready to import as ${protocol.toUpperCase()} by default.` : "Supported formats: host:port, authenticated HTTP, SOCKS4, and SOCKS5.")}</span><button className="button outline" type="button" onClick={() => setProxyView("list")} disabled={running}>Cancel</button><button className="button primary" onClick={add} type="button" disabled={running || !raw.trim() || parsedAccepted === 0}><Plus size={14} />{running ? "Importing…" : "Add proxies"}</button></footer>
        </div>
      )}
    </section>
  );
}

function formatUpdateDate(value: string) {
  const parsed = new Date(value);
  if (Number.isNaN(parsed.getTime())) return value;
  return parsed.toLocaleDateString("en-US", { month: "short", day: "numeric", year: "numeric" });
}

function formatUpdateProgress(downloadedBytes: number, totalBytes: number | null) {
  const format = (bytes: number) => {
    if (bytes < 1024 * 1024) return `${Math.max(0, bytes / 1024).toFixed(0)} KB`;
    return `${(bytes / (1024 * 1024)).toFixed(1)} MB`;
  };

  return totalBytes ? `${format(downloadedBytes)} of ${format(totalBytes)}` : `${format(downloadedBytes)} downloaded`;
}

function Settings({
  active,
  settings,
  configuredModule,
  installedVersion,
  updateState,
  onCheckForUpdates,
  onInstallUpdate,
  onSaved,
}: {
  active: boolean;
  settings: AppSettings | null;
  configuredModule: ModuleInfo | null;
  installedVersion: string;
  updateState: AppUpdateState;
  onCheckForUpdates: () => void;
  onInstallUpdate: () => void;
  onSaved: (settings: AppSettings) => void;
}) {
  const [threads, setThreads] = useState(settings?.threads ?? 24);
  const [moduleThreads, setModuleThreads] = useState<Record<string, number>>(settings?.moduleThreads ?? {});
  const [delayMs, setDelayMs] = useState(settings?.delayMs ?? 120);
  const [timeoutMs, setTimeoutMs] = useState(settings?.timeoutMs ?? 10_000);
  const [retries, setRetries] = useState(settings?.retries ?? 1);
  const [maxScanDirectories, setMaxScanDirectories] = useState(settings?.maxScanDirectories ?? 0);
  const [maxScanFiles, setMaxScanFiles] = useState(settings?.maxScanFiles === null ? 0 : settings?.maxScanFiles ?? DEFAULT_MAX_SCAN_FILES);
  const [scanBudgetMib, setScanBudgetMib] = useState(settings?.scanBudgetMib === null ? 0 : settings?.scanBudgetMib ?? DEFAULT_SCAN_BUDGET_MIB);
  const [autoCheckUpdates, setAutoCheckUpdates] = useState(settings?.autoCheckUpdates ?? true);
  const [autoInstallUpdates, setAutoInstallUpdates] = useState(settings?.autoInstallUpdates ?? true);
  const [message, setMessage] = useState("");

  useEffect(() => {
    if (!settings) return;
    setThreads(settings.threads);
    setModuleThreads(settings.moduleThreads);
    setDelayMs(settings.delayMs);
    setTimeoutMs(settings.timeoutMs);
    setRetries(settings.retries);
    setMaxScanDirectories(settings.maxScanDirectories ?? 0);
    setMaxScanFiles(settings.maxScanFiles === null ? 0 : settings.maxScanFiles ?? DEFAULT_MAX_SCAN_FILES);
    setScanBudgetMib(settings.scanBudgetMib === null ? 0 : settings.scanBudgetMib ?? DEFAULT_SCAN_BUDGET_MIB);
    setAutoCheckUpdates(settings.autoCheckUpdates ?? true);
    setAutoInstallUpdates(settings.autoInstallUpdates ?? true);
  }, [settings]);

  async function save() {
    if (!settings) return;
    try {
      const saved = await invoke<AppSettings>("save_settings", { settings: { ...settings, threads, moduleThreads, delayMs, timeoutMs, retries, maxScanDirectories: maxScanDirectories === 0 ? null : maxScanDirectories, maxScanFiles: maxScanFiles === 0 ? null : maxScanFiles, scanBudgetMib: scanBudgetMib === 0 ? null : scanBudgetMib, autoCheckUpdates, autoInstallUpdates } });
      onSaved(saved);
      setMessage("Settings saved locally.");
    } catch (reason: unknown) {
      setMessage(String(reason));
    }
  }

  return (
    <section className={configuredModule ? "settings-page has-module-settings" : "settings-page"} hidden={!active}>
      <header className="settings-page-header">
        <h1>Configuration</h1>
        <p>Task execution, network requests, and local discovery.</p>
      </header>

      <div className="settings-sections">
        {configuredModule && (
          <section className="settings-section" aria-labelledby="module-settings-title">
            <h2 className="settings-module-title" id="module-settings-title"><span><ModuleBrandIcon id={configuredModule.id} /></span>{configuredModule.name}</h2>
            <div className="settings-group">
              <SettingNumberRow
                id={`module-workers-${configuredModule.id}`}
                label="Module workers"
                description="Concurrent validation workers for this module (maximum 32)"
                min={1}
                max={32}
                value={moduleThreads[configuredModule.id] ?? threads}
                onChange={(value) => setModuleThreads((current) => ({ ...current, [configuredModule.id]: value }))}
              />
            </div>
          </section>
        )}

        <section className="settings-section" aria-labelledby="execution-settings-title">
          <h2 id="execution-settings-title">Execution</h2>
          <div className="settings-group">
            <SettingNumberRow id="threads" label="Default workers" description="Proxy checks may use up to 200; validation tasks are capped at 32" min={1} max={200} value={threads} onChange={setThreads} />
            <SettingNumberRow id="delay-ms" label="Delay between items" description="Milliseconds between items per worker" min={0} max={60_000} value={delayMs} onChange={setDelayMs} />
          </div>
        </section>

        <section className="settings-section" aria-labelledby="request-settings-title">
          <h2 id="request-settings-title">Requests</h2>
          <div className="settings-group">
            <SettingNumberRow id="timeout-ms" label="HTTP timeout" description="Milliseconds allowed for each request" min={3_000} max={120_000} value={timeoutMs} onChange={setTimeoutMs} />
            <SettingNumberRow id="retries" label="Retries" description="Retries after a transient failure" min={0} max={5} value={retries} onChange={setRetries} />
          </div>
        </section>

        <section className="settings-section" aria-labelledby="discovery-settings-title">
          <h2 id="discovery-settings-title">Discovery</h2>
          <div className="settings-group">
            <SettingNumberRow id="max-scan-directories" label="Directory limit" description="Maximum directories visited while expanding folders" min={1} max={10_000} value={maxScanDirectories} onChange={setMaxScanDirectories} unlimitedDefault={DEFAULT_MAX_SCAN_DIRECTORIES} unlimitedDescription="No directory-count cap. Traversal streams entries without retaining the full directory tree." />
            <SettingNumberRow id="max-scan-files" label="File limit" description="Maximum unique file paths discovered for each task" min={1} max={100_000} value={maxScanFiles} onChange={setMaxScanFiles} unlimitedDefault={DEFAULT_MAX_SCAN_FILES} unlimitedDescription="No discovered-file-count cap. Files stream through a bounded worker queue without retaining the full list." />
            <SettingNumberRow id="scan-budget-mib" label="Scan budget" description="Aggregate MiB available to each local validation phase" min={1} max={4_096} value={scanBudgetMib} onChange={setScanBudgetMib} unlimitedDefault={DEFAULT_SCAN_BUDGET_MIB} unlimitedDescription="No aggregate byte cap. Files flow through a bounded worker queue." />
          </div>
        </section>

        <section className="settings-section" aria-labelledby="update-settings-title">
          <h2 id="update-settings-title">Updates</h2>
          <div className="settings-group update-settings-group">
            <div className="settings-row update-version-row">
              <div className="settings-copy">
                <strong>Installed version</strong>
                <small role="status" aria-live="polite">{updateState.message}</small>
              </div>
              <div className="update-version-summary">
                <span className={`update-status update-status-${updateState.phase}`}>{updatePhaseLabels[updateState.phase]}</span>
                <strong>v{installedVersion}</strong>
              </div>
            </div>

            <div className="settings-row">
              <div className="settings-copy">
                <strong>Automatically check for updates</strong>
                <small>
                  {autoCheckUpdates
                    ? "Looks for a newer signed release when Ayla starts."
                    : "Ayla never looks for updates on its own."}
                </small>
              </div>
              <button
                className={autoCheckUpdates ? "switch-control active" : "switch-control"}
                type="button"
                role="switch"
                aria-checked={autoCheckUpdates}
                aria-label="Automatically check for updates"
                onClick={() => setAutoCheckUpdates((current) => !current)}
                disabled={!settings}
              >
                <span />
              </button>
            </div>

            <div className="settings-row">
              <div className="settings-copy">
                <strong>Install updates without asking</strong>
                <small>No approval needed: Ayla warns you, counts down, then downloads, installs and restarts. You can postpone. A running task or proxy check always finishes first.</small>
              </div>
              <button
                className={autoInstallUpdates ? "switch-control active" : "switch-control"}
                type="button"
                role="switch"
                aria-checked={autoInstallUpdates}
                aria-label="Install updates without asking"
                onClick={() => setAutoInstallUpdates((current) => !current)}
                disabled={!settings}
              >
                <span />
              </button>
            </div>

            <div className="settings-row update-check-row">
              <div className="settings-copy">
                <strong>Check manually</strong>
                <small>Looks for the newest signed stable release without installing it.</small>
              </div>
              <button
                className="button outline update-check-button"
                type="button"
                onClick={onCheckForUpdates}
                disabled={["checking", "downloading", "installing", "restarting"].includes(updateState.phase)}
              >
                <RefreshCw className={updateState.phase === "checking" ? "is-spinning" : ""} size={14} />
                {updateState.phase === "checking" ? "Checking…" : "Check for updates"}
              </button>
            </div>

            {updateState.version && (
              <div className="update-release-panel">
                <div className="update-release-heading">
                  <div>
                    <span>Available update</span>
                    <strong>Ayla {updateState.version}</strong>
                    {updateState.publishedAt && <small>{formatUpdateDate(updateState.publishedAt)}</small>}
                  </div>
                  <button
                    className="button primary"
                    type="button"
                    onClick={onInstallUpdate}
                    disabled={["downloading", "installing", "restarting"].includes(updateState.phase)}
                  >
                    <Download size={14} />
                    {updateState.phase === "downloading" ? "Downloading…" : updateState.phase === "installing" ? "Installing…" : updateState.phase === "restarting" ? "Restarting…" : updateState.phase === "error" && updateState.downloadComplete ? "Retry install" : updateState.downloadComplete ? "Install and restart" : "Update and restart"}
                  </button>
                </div>

                <div className="update-release-notes">
                  <strong>What’s new</strong>
                  <p>{updateState.notes || "No release notes were provided for this version."}</p>
                </div>

                {(updateState.phase === "downloading" || updateState.downloadComplete) && (
                  <div className="update-download-progress" aria-live="polite">
                    <div>
                      <span>{updateState.downloadComplete ? "Download complete" : "Downloading update"}</span>
                      <strong>{formatUpdateProgress(updateState.downloadedBytes, updateState.totalBytes)}</strong>
                    </div>
                    {updateState.totalBytes ? (
                      <progress aria-label="Update download progress" value={Math.min(updateState.downloadedBytes, updateState.totalBytes)} max={updateState.totalBytes} />
                    ) : (
                      <progress aria-label="Update download progress" />
                    )}
                  </div>
                )}
              </div>
            )}
          </div>
        </section>

      </div>

      <footer className="settings-savebar">
        <span>{message}</span>
        <button className="button primary" onClick={save} type="button" disabled={!settings}>Save changes</button>
      </footer>
    </section>
  );
}

function SettingNumberRow({ id, label, description, min, max, value, onChange, disabled = false, unlimitedDefault, unlimitedDescription }: { id: string; label: string; description: string; min: number; max: number; value: number; onChange: (value: number) => void; disabled?: boolean; unlimitedDefault?: number; unlimitedDescription?: string }) {
  const supportsUnlimited = unlimitedDefault !== undefined;
  const isUnlimited = supportsUnlimited && value === 0;
  const descriptionId = `${id}-description`;

  return (
    <div className={supportsUnlimited ? "settings-row settings-unlimited-row" : "settings-row"}>
      <div className="settings-copy">
        <label htmlFor={id}>{label}</label>
        <small id={descriptionId}>{isUnlimited ? unlimitedDescription ?? `${description} · Unlimited is enabled` : description}</small>
      </div>
      <div className={isUnlimited ? "settings-number-control is-unlimited" : "settings-number-control"}>
        <input
          className="field-input number-input"
          id={id}
          type="number"
          min={min}
          max={max}
          value={isUnlimited ? "" : value}
          placeholder={isUnlimited ? "Unlimited" : undefined}
          onChange={(event) => {
            const nextValue = event.currentTarget.valueAsNumber;
            if (Number.isFinite(nextValue)) onChange(nextValue);
          }}
          disabled={disabled || isUnlimited}
          aria-describedby={descriptionId}
        />
        {supportsUnlimited && (
          <button
            className={isUnlimited ? "button outline small settings-unlimited-toggle active" : "button outline small settings-unlimited-toggle"}
            type="button"
            aria-label={`${label}: Unlimited`}
            aria-pressed={isUnlimited}
            aria-controls={id}
            onClick={() => onChange(isUnlimited ? unlimitedDefault : 0)}
            disabled={disabled}
          >
            Unlimited
          </button>
        )}
      </div>
    </div>
  );
}

export default App;
