import { useEffect, useMemo, useRef, useState, type RefObject } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { open } from "@tauri-apps/plugin-dialog";
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
  ChevronDown,
  CarFront,
  CircleHelp,
  CircleDot,
  CircleGauge,
  Clock3,
  Disc3,
  Ellipsis,
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
import "./App.css";

const MAX_PROXY_IMPORT_BYTES = 16 * 1024 * 1024;

type Page = "overview" | "modules" | "tasks" | "proxies" | "settings";

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
  maxScanDirectories: number;
  maxScanFiles: number;
  scanBudgetMib: number;
}

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
  status: "pending" | "live";
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
  removed: number;
  id: string;
  item: ProxyItem | null;
  status: "running" | "live" | "dead" | "stopped" | "done";
  running: boolean;
}
interface ChatGptTaskSummary {
  active: number;
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
  version: "0.1.0",
  modulesTotal: 14,
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
  ["kick", "Kick", "Social", "Live-streaming and creator platform"],
  ["instagram", "Instagram", "Social", "Photo, video, and social networking platform"],
  ["reddit", "Reddit", "Social", "Community discussion and content-sharing platform"],
].map(([id, name, category, description]) => ({ id, name, category, description, enabled: id === "chatgpt" || id === "twitch" }));

function App() {
  const [navigation, setNavigation] = useState<{ entries: Page[]; index: number }>({ entries: ["overview"], index: 0 });
  const [sidebarOpen, setSidebarOpen] = useState(true);
  const [aboutOpen, setAboutOpen] = useState(false);
  const [overview, setOverview] = useState<AppOverview | null>(fallbackOverview);
  const [modules, setModules] = useState<ModuleInfo[]>(fallbackModules);
  const [settings, setSettings] = useState<AppSettings | null>(null);
  const [systemMetrics, setSystemMetrics] = useState<SystemMetrics | null>(null);
  const [taskSnapshot, setTaskSnapshot] = useState<TaskSnapshot | null>(null);
  const [taskHistory, setTaskHistory] = useState<TaskHistoryEntry[]>([]);
  const [configuredModuleId, setConfiguredModuleId] = useState<string | null>(null);
  const [modulePreferences, setModulePreferences] = useState<Record<string, boolean>>(() => {
    try {
      return JSON.parse(localStorage.getItem("ayla.module-preferences") ?? "{}") as Record<string, boolean>;
    } catch {
      return {};
    }
  });
  const [error, setError] = useState("");
  const searchInputRef = useRef<HTMLInputElement>(null);
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

  const refreshTaskHistory = async () => {
    try {
      const nextHistory = await invoke<TaskHistoryEntry[]>("task_history", { limit: 100 });
      setTaskHistory(nextHistory);
    } catch {
      // Browser previews do not expose the Tauri command bridge.
    }
  };

  useEffect(() => {
    Promise.all([
      invoke<AppOverview>("get_app_overview"),
      invoke<ModuleInfo[]>("list_modules"),
      invoke<AppSettings>("get_settings"),
    ])
      .then(([nextOverview, nextModules, nextSettings]) => {
        setOverview(nextOverview);
        setModules(nextModules);
        setSettings(nextSettings);
      })
      .catch((reason: unknown) => setError(String(reason)));
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
      if (event.key === "Escape") setAboutOpen(false);
    };
    window.addEventListener("keydown", onKeyDown);
    return () => window.removeEventListener("keydown", onKeyDown);
  }, []);

  useEffect(() => {
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
  }, []);

  useEffect(() => {
    let active = true;
    const cleanups: Array<() => void> = [];
    const receive = (snapshot: TaskSnapshot) => {
      if (!active) return;
      setTaskSnapshot((current) => {
        if (current?.runId === snapshot.runId && current.sequence > snapshot.sequence) return current;
        if (current && current.runId !== snapshot.runId && taskIsRunning(current)) return current;
        return snapshot;
      });
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
  }, []);

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
        onAbout={() => setAboutOpen(true)}
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
          />
        )}

        <div className="main-column">
          <main className={`content page-${page}`}>
            {error && (
              <div className="notice danger-notice" title={error}>
                <CircleDot size={15} />
                <div><strong>Desktop services are unavailable in this preview</strong><span>Open Ayla through Tauri to use local data.</span></div>
              </div>
            )}
            {page === "overview" && <Overview overview={overview} metrics={systemMetrics} history={taskHistory} modules={modules} />}
            {page === "modules" && (
              <Modules
                modules={modules}
                preferences={modulePreferences}
                onToggle={(id) => setModulePreferences((current) => ({ ...current, [id]: !(current[id] ?? true) }))}
                onConfigure={(id) => { setConfiguredModuleId(id); navigate("settings"); }}
              />
            )}
            {page === "tasks" && (
              <Tasks
                modules={runnableModules}
                defaultConcurrency={settings?.threads ?? 24}
                moduleConcurrency={settings?.moduleThreads}
                defaultDelayMs={settings?.delayMs ?? 120}
                proxiesLive={overview?.proxiesLive ?? 0}
                onOpenProxies={() => navigate("proxies")}
                onTaskSnapshot={setTaskSnapshot}
                onHistoryChanged={refreshTaskHistory}
              />
            )}
            {page === "proxies" && (
              <Proxies
                defaultThreads={settings?.threads ?? 24}
                defaultTimeoutMs={settings?.timeoutMs ?? 15_000}
                onCountsChanged={(proxiesTotal, proxiesLive) => {
                  setOverview((current) => current ? { ...current, proxiesTotal, proxiesLive } : current);
                }}
              />
            )}
            {page === "settings" && <Settings settings={settings} configuredModule={modules.find((item) => item.id === configuredModuleId) ?? null} onSaved={setSettings} />}
          </main>
        </div>

      </div>

      {aboutOpen && (
        <div className="modal-backdrop" role="presentation" onMouseDown={() => setAboutOpen(false)}>
          <article className="about-dialog" role="dialog" aria-modal="true" aria-labelledby="about-title" onMouseDown={(event) => event.stopPropagation()}>
            <div className="about-mark"><Flower2 size={22} /></div>
            <h2 id="about-title">Ayla</h2>
            <p>Version {overview?.version ?? "0.1.0"}</p>
            <button className="button secondary" type="button" onClick={() => setAboutOpen(false)}>Close</button>
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
          <button className={openMenu === "file" ? "titlebar-menu-button active" : "titlebar-menu-button"} type="button" aria-haspopup="menu" aria-expanded={openMenu === "file"} onClick={() => setOpenMenu((current) => current === "file" ? null : "file")}>File</button>
          {openMenu === "file" && <div className="titlebar-dropdown" role="menu"><button role="menuitem" type="button" onClick={() => choose(() => onNavigate("overview"))}>Open Overview</button><button role="menuitem" type="button" onClick={() => choose(() => onNavigate("tasks"))}>New Task</button><span className="menu-separator" /><button role="menuitem" type="button" onClick={() => choose(() => runWindowAction((window) => window.close()))}>Exit Ayla</button></div>}
        </div>
        <div className="titlebar-menu-group">
          <button className={openMenu === "edit" ? "titlebar-menu-button active" : "titlebar-menu-button"} type="button" aria-haspopup="menu" aria-expanded={openMenu === "edit"} onClick={() => setOpenMenu((current) => current === "edit" ? null : "edit")}>Edit</button>
          {openMenu === "edit" && <div className="titlebar-dropdown" role="menu"><button role="menuitem" type="button" onClick={() => choose(onSearch)}>Search <kbd>Ctrl K</kbd></button><button role="menuitem" type="button" onClick={() => choose(() => onNavigate("settings"))}>Settings</button></div>}
        </div>
        <div className="titlebar-menu-group">
          <button className={openMenu === "view" ? "titlebar-menu-button active" : "titlebar-menu-button"} type="button" aria-haspopup="menu" aria-expanded={openMenu === "view"} onClick={() => setOpenMenu((current) => current === "view" ? null : "view")}>View</button>
          {openMenu === "view" && <div className="titlebar-dropdown" role="menu"><button role="menuitem" type="button" onClick={() => choose(onToggleSidebar)}>{sidebarOpen ? "Hide" : "Show"} Sidebar</button></div>}
        </div>
        <div className="titlebar-menu-group">
          <button className={openMenu === "help" ? "titlebar-menu-button active" : "titlebar-menu-button"} type="button" aria-haspopup="menu" aria-expanded={openMenu === "help"} onClick={() => setOpenMenu((current) => current === "help" ? null : "help")}>Help</button>
          {openMenu === "help" && <div className="titlebar-dropdown" role="menu"><button role="menuitem" type="button" onClick={() => choose(onAbout)}><Info size={14} /> About Ayla</button></div>}
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
}: {
  page: Page;
  onNavigate: (page: Page) => void;
  overview: AppOverview | null;
  modules: ModuleInfo[];
  taskSnapshot: TaskSnapshot | null;
  searchInputRef: RefObject<HTMLInputElement | null>;
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
      <div className="sidebar-search">
        <Search size={14} />
        <input
          ref={searchInputRef}
          value={query}
          onChange={(event) => setQuery(event.target.value)}
          onFocus={() => setSearchOpen(true)}
          onBlur={() => window.setTimeout(() => setSearchOpen(false), 120)}
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
              {item.id === "modules" && <small>{overview?.modulesTotal ?? 13}</small>}
              {item.id === "proxies" && Boolean(overview?.proxiesTotal) && <small>{overview?.proxiesTotal}</small>}
            </button>
          );
        })}
      </nav>

      <div className="sidebar-spacer" />
      <div className="sidebar-runtime">
        <div><Activity size={14} /><span>Task progress</span><strong>{Math.round(progress)}%</strong></div>
        <div className="runtime-track" role="progressbar" aria-label="Task progress" aria-valuemin={0} aria-valuemax={100} aria-valuenow={Math.round(progress)}><span style={{ width: `${progress}%` }} /></div>
        <small>{taskSnapshot ? `${taskDone} of ${taskSnapshot.total} · ${taskStatusLabel(taskSnapshot.status)}` : "No active task"}</small>
      </div>
      <div className="sidebar-profile-wrap" ref={profileRoot}>
        {profilePopover === "profile" && (
          <div className="profile-popover" ref={profilePopoverRef} role="dialog" aria-label="Profile menu">
            <div className="profile-popover-header"><span className="avatar">US</span><div><strong>Username</strong><small>@username</small></div><span className="profile-plan-badge">Super</span></div>
            <button className="profile-menu-item" type="button" aria-expanded={planExpanded} onClick={() => setPlanExpanded((current) => !current)}><CircleGauge size={16} /><span>Plan information</span><ChevronDown className={planExpanded ? "expanded" : ""} size={15} /></button>
            {planExpanded && (
              <div className="profile-plan-details">
                <div className="profile-plan-current"><span>Current plan</span><strong>Super</strong></div>
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
            <button className="profile-menu-item danger" type="button" onClick={() => setProfileMessage("No account session is connected in this local build.")}><LogOut size={16} /><span>Log out</span></button>
            {profileMessage && <p className="profile-menu-message" role="status">{profileMessage}</p>}
          </div>
        )}

        {profilePopover === "help" && (
          <div className="profile-popover profile-help-popover" ref={profilePopoverRef} role="dialog" aria-label="Help menu">
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
            <span className="avatar">US</span>
            <span><strong>Username</strong><small>Super</small></span>
          </button>
          <button className="profile-help-trigger" ref={helpTriggerRef} type="button" aria-label="Open help menu" aria-haspopup="dialog" aria-expanded={profilePopover === "help"} onClick={() => { setProfilePopover((current) => current === "help" ? null : "help"); setHelpDetail(null); }}><CircleHelp size={17} /></button>
        </div>
      </div>
    </aside>
  );
}

function Overview({ overview, metrics, history, modules }: { overview: AppOverview | null; metrics: SystemMetrics | null; history: TaskHistoryEntry[]; modules: ModuleInfo[] }) {
  const memoryPercent = metrics?.memoryTotalBytes ? (metrics.memoryUsedBytes / metrics.memoryTotalBytes) * 100 : 0;
  const sessionsChecked = history.reduce((sum, item) => sum + item.total, 0);
  const sessionsSucceeded = history.reduce((sum, item) => sum + item.succeeded, 0);
  const successRate = sessionsChecked ? (sessionsSucceeded / sessionsChecked) * 100 : null;
  const proxyAvailability = overview?.proxiesTotal ? ((overview.proxiesLive / overview.proxiesTotal) * 100) : null;
  const averageTaskSize = history.length ? sessionsChecked / history.length : null;
  const today = new Intl.DateTimeFormat("en-US", { month: "long", day: "numeric", year: "numeric" }).format(new Date());

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
    <section className="overview-page">
      <header className="profile-hero">
        <div className="profile-avatar" aria-hidden="true">US</div>
        <div className="profile-identity">
          <h1>Username</h1>
          <p>@username <span className="profile-plan">Super</span></p>
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
        <h2>Task activity</h2>
        <span className="contribution-summary">{total} {total === 1 ? "task" : "tasks"} in the last 12 months</span>
      </div>
      <div className="contribution-scroll">
        <div className="heatmap-grid">
          {weeks.flatMap((week) => week.map((date) => {
            const count = activity.get(localDayKey(date)) ?? 0;
            const level = count === 0 ? 0 : Math.max(1, Math.ceil((count / maximum) * 4));
            const label = `${new Intl.DateTimeFormat("en-US", { month: "short", day: "numeric", year: "numeric" }).format(date)}: ${count} ${count === 1 ? "task" : "tasks"}`;
            return <span className={`heatmap-cell level-${level}`} key={localDayKey(date)} title={label} aria-label={label} />;
          }))}
        </div>
        <div className="heatmap-months">
          {monthLabels.map((label, index) => <span key={`${label}-${index}`}>{label}</span>)}
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
  { id: "media", title: "Media & communities", modules: ["spotify", "twitch", "kick", "instagram", "reddit"] },
];

function Modules({ modules, preferences, onToggle, onConfigure }: { modules: ModuleInfo[]; preferences: Record<string, boolean>; onToggle: (id: string) => void; onConfigure: (id: string) => void }) {
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
    <section className="modules-page">
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

function Tasks({ modules, defaultConcurrency, moduleConcurrency, defaultDelayMs, proxiesLive, onOpenProxies, onTaskSnapshot, onHistoryChanged }: { modules: ModuleInfo[]; defaultConcurrency: number; moduleConcurrency?: Record<string, number>; defaultDelayMs: number; proxiesLive: number; onOpenProxies: () => void; onTaskSnapshot: (snapshot: TaskSnapshot) => void; onHistoryChanged: () => void }) {
  const enabledModules = useMemo(() => modules.filter((module) => module.enabled), [modules]);
  const preferredModuleId = enabledModules.find((module) => module.id === "chatgpt")?.id ?? enabledModules[0]?.id ?? "";
  const [moduleId, setModuleId] = useState(preferredModuleId);
  const [rawEntries, setRawEntries] = useState("");
  const [concurrency, setConcurrency] = useState(Math.max(1, Math.min(32, defaultConcurrency)));
  const [delayMs, setDelayMs] = useState(defaultDelayMs);
  const [useProxy, setUseProxy] = useState(false);
  const [outputDirectory, setOutputDirectory] = useState("");
  const [selectingOutputDirectory, setSelectingOutputDirectory] = useState(false);
  const [activeTask, setActiveTask] = useState<TaskSnapshot | null>(null);
  const [history, setHistory] = useState<TaskHistoryEntry[]>([]);
  const [starting, setStarting] = useState(false);
  const [cancelling, setCancelling] = useState(false);
  const [message, setMessage] = useState("");
  const [taskView, setTaskView] = useState<"history" | "create">("history");
  const [historyPage, setHistoryPage] = useState(0);

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

  async function reloadHistory() {
    try {
      setHistory(await invoke<TaskHistoryEntry[]>("task_history", { limit: 30 }));
      setHistoryPage(0);
      onHistoryChanged();
    } catch {
      // The global notice already covers browser previews without Tauri.
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
          if (payload.historyPersisted === false) {
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
        `${snapshot.total.toLocaleString("en-US")} structurally usable authentication ${snapshot.total === 1 ? "file" : "files"} found`,
        snapshot.locallyFiltered > 0 ? `${snapshot.locallyFiltered.toLocaleString("en-US")} unrelated or unusable ${snapshot.locallyFiltered === 1 ? "file" : "files"} ignored locally` : "",
        duplicates > 0 ? `${duplicates} duplicate source ${duplicates === 1 ? "path" : "paths"} removed` : "",
        outputDirectory ? `Results will be copied to ${outputFolderName}` : "",
      ].filter(Boolean);
      setMessage(`${preparationNotes.join(" · ")}. Authenticated validation started.`);
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
    try {
      await invoke("clear_task_history");
      setHistory([]);
      setHistoryPage(0);
      onHistoryChanged();
    } catch {
      setMessage("The task history could not be cleared.");
    }
  }

  return (
    <section className="tasks-page">
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
              <div className="task-active-progress"><div><span>{Math.max(0, activeTask.total - activeTask.queued - activeTask.running)} of {activeTask.total}</span><strong>{Math.round(progress)}%</strong></div><progress className="task-progress" value={progress} max="100" /></div>
              <div className="task-active-stats">
                <span>{activeTask.queued} queued</span><span>{activeTask.running} running</span><span>{outcomeSummary?.active ?? activeTask.succeeded} active</span><span>{outcomeSummary?.dead ?? activeTask.failed} {hasDetailedOutcome ? "dead" : "failed"}</span>{moduleSummary && moduleSummary.rateLimited > 0 && <span>{moduleSummary.rateLimited} rate limited</span>}{moduleSummary && moduleSummary.errors > 0 && <span>{moduleSummary.errors} errors</span>}{moduleSummary && moduleSummary.invalid > 0 && <span>{moduleSummary.invalid} invalid</span>}{activeTask.locallyFiltered > 0 && <span>{activeTask.locallyFiltered} ignored locally</span>}<span>{activeTask.skipped} skipped</span><span>{activeTask.retried} retries</span><span>{activeTask.useProxy ? `${activeTask.proxyCount} proxy pool` : "Direct"}</span>{activeTask.resultsExportEnabled && <><span>{activeTask.exportedActive ?? 0} active copied</span><span>{activeTask.exportedFailed ?? 0} failed copied</span>{(activeTask.exportErrors ?? 0) > 0 && <span className="task-export-error">{activeTask.exportErrors} copy errors</span>}</>}
              </div>
              <button className="button danger task-active-cancel" type="button" onClick={cancel} disabled={cancelling}><StopCircle size={14} />{cancelling ? "Cancelling…" : "Cancel"}</button>
            </article>
          )}

          {message && <p className="form-message task-page-message">{message}</p>}

          <section className="task-history-panel" aria-labelledby="task-history-title">
            <header className="task-history-header"><div><h2 id="task-history-title">History</h2><p>{history.length} recorded {history.length === 1 ? "task" : "tasks"}</p></div></header>
            {history.length === 0 ? (
              <div className="empty-state task-history-empty"><Clock3 size={21} /><strong>No tasks yet</strong><span>Create a task to begin.</span></div>
            ) : (
              <div className="task-history-list">
                {visibleHistory.map((task) => (
                  <article className="task-history-row" key={task.runId}>
                    <span className="task-history-module"><ModuleBrandIcon id={task.moduleId} /></span>
                    <div className="task-history-copy"><strong>{moduleById.get(task.moduleId)?.name ?? task.moduleId}</strong><small>{formatTaskDate(task.finishedAt ?? task.startedAt)} · {task.runId.slice(0, 8)} · {task.useProxy ? `${task.proxyCount ?? 0} proxies` : "Direct"} · {task.concurrency} workers</small></div>
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
                  <div className="task-module-picker" role="radiogroup" aria-label="Enabled modules">
                    {enabledModules.map((module) => <button className={moduleId === module.id ? "task-module-choice active" : "task-module-choice"} type="button" role="radio" aria-checked={moduleId === module.id} key={module.id} onClick={() => setModuleId(module.id)} disabled={starting}><span><ModuleBrandIcon id={module.id} /></span>{module.name}</button>)}
                  </div>
                </div>
                <div className="settings-row task-path-row">
                  <div className="settings-copy"><label htmlFor="task-entries">Authorized files or folders</label><small>Paste one local path per line. Duplicates are removed automatically.</small></div>
                  <textarea className={`field-input task-entry-input${entryLimitExceeded ? " invalid" : ""}`} id="task-entries" value={rawEntries} onChange={(event) => setRawEntries(event.target.value)} placeholder={"C:\\Ayla\\examples"} disabled={starting} spellCheck={false} aria-invalid={entryLimitExceeded} />
                </div>
                <div className="settings-row task-source-row"><div className="settings-copy"><strong>Source preview</strong><small>{entryLimitExceeded ? "The 10,000-path limit has been exceeded." : duplicateEntryCount ? `${duplicateEntryCount} duplicate path(s) will be removed.` : "Cookie totals are calculated after the local scan starts."}</small></div><div className="task-source-summary"><span><strong>{uniqueEntryCount.toLocaleString("en-US")}</strong>Paths</span><span><strong>—</strong>Cookies</span></div></div>
                <div className="settings-row task-output-row">
                  <div className="settings-copy"><strong>Results folder <span className="optional-label">Optional</span></strong><small>{`Copies results into ${selectedModule?.name ?? "Module"}/active and ${selectedModule?.name ?? "Module"}/failed. Original files stay untouched.`}</small></div>
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
                <div className="settings-row task-proxy-setting"><div className="settings-copy"><strong>Proxy routing</strong><small>{proxiesLive > 0 ? `${proxiesLive} live ${proxiesLive === 1 ? "proxy" : "proxies"} available` : "Add and check proxies before enabling this option."}</small>{proxiesLive === 0 && <button className="inline-action" type="button" onClick={onOpenProxies}>Manage proxies</button>}</div><button className={useProxy ? "switch-control active" : "switch-control"} type="button" role="switch" aria-checked={useProxy} onClick={() => setUseProxy((current) => !current)} disabled={starting || proxiesLive === 0}><span /></button></div>
                <SettingNumberRow id="task-concurrency" label="Concurrency" description={useProxy && effectiveConcurrency < requestedConcurrency ? `${requestedConcurrency} requested · ${effectiveConcurrency} effective with the current proxy pool.` : "Parallel workers. Higher values use more CPU and network capacity."} min={1} max={32} value={concurrency} onChange={setConcurrency} disabled={starting} />
                <SettingNumberRow id="task-delay" label={selectedModule?.id === "twitch" ? "Request spacing" : "Worker delay"} description={selectedModule?.id === "twitch" ? "Minimum global spacing between Twitch requests, including retries and proxy failover." : "Pause between entries per worker, in milliseconds."} min={0} max={60_000} value={delayMs} onChange={setDelayMs} disabled={starting} />
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

function Proxies({ defaultThreads, defaultTimeoutMs, onCountsChanged }: { defaultThreads: number; defaultTimeoutMs: number; onCountsChanged: (total: number, live: number) => void }) {
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
  const [proxyFilter, setProxyFilter] = useState<"all" | "live" | "pending">("all");
  const [query, setQuery] = useState("");
  const [proxyPage, setProxyPage] = useState(0);
  const [proxyPageSize, setProxyPageSize] = useState(5);
  const [running, setRunning] = useState(false);
  const [message, setMessage] = useState("");
  const proxyFileInputRef = useRef<HTMLInputElement>(null);
  const proxyListRef = useRef<HTMLDivElement>(null);
  const selectedIdSet = useMemo(() => new Set(selectedIds), [selectedIds]);

  function applyItems(nextItems: ProxyItem[]) {
    itemsRef.current = nextItems;
    const availableIds = new Set(nextItems.map((item) => item.id));
    setItems(nextItems);
    setSelectedIds((current) => current.filter((id) => availableIds.has(id)));
    setSelectedProxyId((current) => current && availableIds.has(current) ? current : nextItems[0]?.id ?? null);
    onCountsChanged(nextItems.length, nextItems.filter((item) => item.status === "live").length);
  }

  useEffect(() => {
    let active = true;
    const cleanups: Array<() => void> = [];

    invoke<ProxyItem[]>("list_proxies").then((nextItems) => active && applyItems(nextItems)).catch(() => undefined);
    invoke<boolean>("is_proxy_check_running").then((value) => active && setRunning(value)).catch(() => undefined);

    void listen<ProxyProgress>("proxy:progress", ({ payload }) => {
      if (!active) return;
      setProgress(payload);
      setRunning(payload.running);
      if (payload.status === "dead" && payload.id) {
        applyItems(itemsRef.current.filter((item) => item.id !== payload.id));
      } else if (payload.item) {
        const exists = itemsRef.current.some((item) => item.id === payload.item?.id);
        applyItems(exists
          ? itemsRef.current.map((item) => item.id === payload.item?.id ? payload.item as ProxyItem : item)
          : [...itemsRef.current, payload.item]);
      }
    }).then((unlisten) => active ? cleanups.push(unlisten) : unlisten()).catch(() => undefined);

    return () => {
      active = false;
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
      applyItems(result.items);
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
    setRunning(true);
    setMessage("");
    setProgress({ done: 0, total, percent: 0, live: 0, removed: 0, id: "", item: null, status: "running", running: true });
    try {
      const result = await invoke<{ results: ProxyItem[]; stopped: boolean }>("check_proxies", { request: { ids, threads: defaultThreads, timeoutMs: defaultTimeoutMs } });
      applyItems(result.results);
      setMessage(result.stopped ? "Check stopped." : "Check completed.");
    } catch (reason: unknown) {
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
    try {
      applyItems(await invoke<ProxyItem[]>("remove_proxies", { ids: [id] }));
    } catch (reason: unknown) {
      setMessage(String(reason));
    }
  }

  async function clear() {
    try {
      applyItems(await invoke<ProxyItem[]>("clear_proxies"));
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

  const liveCount = items.filter((item) => item.status === "live").length;
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
    <section className="proxies-page">
      {proxyView === "list" ? (
        <div className="proxy-browser-view">
          <section className="proxy-list-pane" aria-label="Saved proxies">
            <div className="proxy-list-tabs" role="tablist" aria-label="Proxy status">
              {(["all", "live", "pending"] as const).map((status) => (
                <button className={proxyFilter === status ? "active" : ""} type="button" role="tab" aria-selected={proxyFilter === status} key={status} onClick={() => { setProxyFilter(status); setProxyPage(0); }}>{status === "all" ? "All" : status === "live" ? "Live" : "Unchecked"}</button>
              ))}
            </div>

            <div className="proxy-search-shell">
              <Search size={15} />
              <input type="search" value={query} onChange={(event) => { setQuery(event.target.value); setProxyPage(0); }} placeholder="Search proxies" aria-label="Search proxies" />
              <button className="proxy-add-pill" type="button" onClick={() => { setMessage(""); setProxyView("add"); }} disabled={running}><Plus size={13} />Add proxy</button>
            </div>

            <div className="proxy-list-summary">
              <span>{filteredItems.length} shown · {liveCount} live</span>
              <div className="proxy-list-summary-actions">
                {running ? <button className="proxy-list-run" onClick={stop} type="button"><StopCircle size={11} />Stop</button> : <button className="proxy-list-run" onClick={() => check(selectedIds)} type="button" disabled={items.length === 0} title={selectedIds.length ? `Check ${selectedIds.length} selected proxies` : "Check every saved proxy"}><Play size={11} />Run checks</button>}
                <button type="button" onClick={() => setSelectedIds(selectedIds.length === items.length ? [] : items.map((item) => item.id))} disabled={running || items.length === 0}>{selectedIds.length === items.length && items.length > 0 ? "Deselect all" : "Select all"}</button>
              </div>
            </div>

            {progress && (
              <div className="proxy-check-progress proxy-check-progress-compact">
                <div><span>{progress.done} of {progress.total}</span><strong>{Math.round(progress.percent)}%</strong></div>
                <progress value={progress.percent} max="100" />
                <small>{progress.live} live · {progress.removed} removed</small>
              </div>
            )}

            {filteredItems.length === 0 ? (
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
                        <span className="proxy-row-meta"><strong>{item.status === "live" ? `${item.latencyMs} ms` : "—"}</strong><small>{item.protocol.toUpperCase()}</small></span>
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
                <button className="button outline small" onClick={clear} type="button" disabled={items.length === 0 || running}><Trash2 size={13} />Clear</button>
              </div>
            </header>

            {progress && (
              <div className="proxy-check-progress">
                <div><span>{progress.done} of {progress.total}</span><strong>{Math.round(progress.percent)}%</strong></div>
                <progress value={progress.percent} max="100" />
                <small>{progress.live} live · {progress.removed} removed</small>
              </div>
            )}

            {message && <p className="proxy-detail-message">{message}</p>}

            {selectedProxy ? (
              <div className="proxy-detail-content">
                <div className="proxy-detail-hero">
                  <span className="proxy-detail-flag">{countryFlag(selectedProxy.countryCode)}</span>
                  <div><h1>{selectedProxy.ip || selectedProxy.host}</h1><p>{[selectedProxy.city, selectedProxy.country].filter(Boolean).join(", ") || "Location unavailable"}</p></div>
                  <span className={`proxy-detail-status ${selectedProxy.status}`}>{selectedProxy.status === "live" ? "Live" : "Unchecked"}</span>
                </div>

                <div className="proxy-detail-grid">
                  <div><span>Address</span><strong>{selectedProxy.host}:{selectedProxy.port}</strong></div>
                  <div><span>Protocol</span><strong>{selectedProxy.protocol.toUpperCase()}</strong></div>
                  <div><span>Latency</span><strong>{selectedProxy.status === "live" ? `${selectedProxy.latencyMs} ms` : "Not checked"}</strong></div>
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

function Settings({ settings, configuredModule, onSaved }: { settings: AppSettings | null; configuredModule: ModuleInfo | null; onSaved: (settings: AppSettings) => void }) {
  const [threads, setThreads] = useState(settings?.threads ?? 24);
  const [moduleThreads, setModuleThreads] = useState<Record<string, number>>(settings?.moduleThreads ?? {});
  const [delayMs, setDelayMs] = useState(settings?.delayMs ?? 120);
  const [timeoutMs, setTimeoutMs] = useState(settings?.timeoutMs ?? 10_000);
  const [retries, setRetries] = useState(settings?.retries ?? 1);
  const [maxScanDirectories, setMaxScanDirectories] = useState(settings?.maxScanDirectories ?? 1_000);
  const [maxScanFiles, setMaxScanFiles] = useState(settings?.maxScanFiles ?? 10_000);
  const [scanBudgetMib, setScanBudgetMib] = useState(settings?.scanBudgetMib ?? 512);
  const [message, setMessage] = useState("");

  useEffect(() => {
    if (!settings) return;
    setThreads(settings.threads);
    setModuleThreads(settings.moduleThreads);
    setDelayMs(settings.delayMs);
    setTimeoutMs(settings.timeoutMs);
    setRetries(settings.retries);
    setMaxScanDirectories(settings.maxScanDirectories ?? 1_000);
    setMaxScanFiles(settings.maxScanFiles ?? 10_000);
    setScanBudgetMib(settings.scanBudgetMib ?? 512);
  }, [settings]);

  async function save() {
    if (!settings) return;
    try {
      const saved = await invoke<AppSettings>("save_settings", { settings: { ...settings, threads, moduleThreads, delayMs, timeoutMs, retries, maxScanDirectories, maxScanFiles, scanBudgetMib } });
      onSaved(saved);
      setMessage("Settings saved locally.");
    } catch (reason: unknown) {
      setMessage(String(reason));
    }
  }

  return (
    <section className={configuredModule ? "settings-page has-module-settings" : "settings-page"}>
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
                description="Concurrent workers used specifically for this module"
                min={1}
                max={200}
                value={moduleThreads[configuredModule.id] ?? threads}
                onChange={(value) => setModuleThreads((current) => ({ ...current, [configuredModule.id]: value }))}
              />
            </div>
          </section>
        )}

        <section className="settings-section" aria-labelledby="execution-settings-title">
          <h2 id="execution-settings-title">Execution</h2>
          <div className="settings-group">
            <SettingNumberRow id="threads" label="Default workers" description="Concurrent workers used by default" min={1} max={200} value={threads} onChange={setThreads} />
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
            <SettingNumberRow id="max-scan-directories" label="Directory limit" description="Maximum directories visited while expanding folders" min={1} max={10_000} value={maxScanDirectories} onChange={setMaxScanDirectories} />
            <SettingNumberRow id="max-scan-files" label="File limit" description="Maximum unique files discovered for each task" min={1} max={100_000} value={maxScanFiles} onChange={setMaxScanFiles} />
            <SettingNumberRow id="scan-budget-mib" label="Scan budget" description="MiB available to each local validation phase" min={1} max={4_096} value={scanBudgetMib} onChange={setScanBudgetMib} />
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

function SettingNumberRow({ id, label, description, min, max, value, onChange, disabled = false }: { id: string; label: string; description: string; min: number; max: number; value: number; onChange: (value: number) => void; disabled?: boolean }) {
  return (
    <div className="settings-row">
      <div className="settings-copy">
        <label htmlFor={id}>{label}</label>
        <small>{description}</small>
      </div>
      <input className="field-input number-input" id={id} type="number" min={min} max={max} value={value} onChange={(event) => onChange(Number(event.target.value))} disabled={disabled} />
    </div>
  );
}

export default App;
