const API_BASE_URL = (
  import.meta.env.VITE_AYLA_API_URL ?? "https://yl.xyne.gg/api/v1"
).replace(/\/+$/, "");
const REQUEST_TIMEOUT_MS = 12_000;

export type AuthUser = {
  id: string;
  name: string;
  email: string;
  role: "user" | "admin";
};

export type AuthSession = {
  token: string;
  expiresAt: string;
  user: AuthUser;
};

export class AuthApiError extends Error {
  code: string;

  constructor(code: string) {
    super(code);
    this.name = "AuthApiError";
    this.code = code;
  }
}

async function request<T>(path: string, init: RequestInit): Promise<T> {
  const controller = new AbortController();
  const timeout = window.setTimeout(() => controller.abort(), REQUEST_TIMEOUT_MS);

  try {
    const response = await fetch(`${API_BASE_URL}${path}`, {
      ...init,
      headers: {
        Accept: "application/json",
        ...(init.body ? { "Content-Type": "application/json" } : {}),
        ...init.headers,
      },
      signal: controller.signal,
    });

    if (!response.ok) {
      const payload = await response.json().catch(() => null) as { error?: { code?: string } } | null;
      throw new AuthApiError(payload?.error?.code || `HTTP_${response.status}`);
    }
    if (response.status === 204) return undefined as T;
    return await response.json() as T;
  } catch (error) {
    if (error instanceof AuthApiError) throw error;
    if (error instanceof DOMException && error.name === "AbortError") {
      throw new AuthApiError("REQUEST_TIMEOUT");
    }
    throw new AuthApiError("NETWORK_ERROR");
  } finally {
    window.clearTimeout(timeout);
  }
}

export async function registerAccount(input: {
  name: string;
  email: string;
  password: string;
}) {
  return request<{ status: "pending"; message: string }>("/auth/register", {
    method: "POST",
    body: JSON.stringify(input),
  });
}

export async function login(input: { email: string; password: string }) {
  return request<AuthSession>("/auth/login", {
    method: "POST",
    body: JSON.stringify(input),
  });
}

export async function logout(token: string) {
  return request<void>("/auth/logout", {
    method: "POST",
    headers: { Authorization: `Bearer ${token}` },
  });
}
