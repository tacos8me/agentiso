import { api } from './client';

// ---------------------------------------------------------------------------
// Backend response types (match Rust VaultManager serialization)
// ---------------------------------------------------------------------------

export interface NoteEntry {
  path: string;
  is_dir: boolean;
  size: number | null;
}

export interface NoteContent {
  path: string;
  content: string;
  frontmatter: Record<string, unknown> | null;
}

export interface SearchHit {
  path: string;
  line_number: number;
  line: string;
  context: string;
}

export interface VaultStats {
  total_notes: number;
  total_size_bytes: number;
  top_tags: Array<{ tag: string; count: number }>;
}

export interface GraphResponse {
  nodes: Array<{ id: string; label: string }>;
  edges: Array<{ source: string; target: string }>;
}

// ---------------------------------------------------------------------------
// API functions
// ---------------------------------------------------------------------------

export function fetchNotes(folder?: string, recursive = true): Promise<NoteEntry[]> {
  const params = new URLSearchParams();
  if (folder) params.set('folder', folder);
  if (!recursive) params.set('recursive', 'false');
  const qs = params.toString();
  return api.get<NoteEntry[]>(`/vault/notes${qs ? `?${qs}` : ''}`);
}

export function readNote(path: string): Promise<NoteContent> {
  return api.get<NoteContent>(`/vault/notes/${path}`);
}

export function writeNote(path: string, content: string): Promise<{ written: string }> {
  return api.put<{ written: string }>(`/vault/notes/${path}`, { content });
}

export function deleteNote(path: string): Promise<{ deleted: string }> {
  return api.delete<{ deleted: string }>(`/vault/notes/${path}`);
}

export function searchVault(
  query: string,
  folder?: string,
  maxResults = 50,
): Promise<SearchHit[]> {
  const params = new URLSearchParams({ query });
  if (folder) params.set('folder', folder);
  params.set('max_results', String(maxResults));
  return api.get<SearchHit[]>(`/vault/search?${params}`);
}

export function getFrontmatter(path: string): Promise<Record<string, unknown> | null> {
  return api.get<Record<string, unknown> | null>(`/vault/frontmatter/${path}`);
}

export function setFrontmatter(
  path: string,
  key: string,
  value: unknown,
): Promise<{ set: string }> {
  return api.put<{ set: string }>(`/vault/frontmatter/${path}`, { key, value });
}

export function getTags(path: string): Promise<string[]> {
  return api.get<string[]>(`/vault/tags/${path}`);
}

export function addTag(path: string, tag: string): Promise<{ added: string }> {
  return api.post<{ added: string }>(`/vault/tags/${path}`, { tag });
}

export function getVaultStats(): Promise<VaultStats> {
  return api.get<VaultStats>('/vault/stats');
}

export function getVaultGraph(): Promise<GraphResponse> {
  return api.get<GraphResponse>('/vault/graph');
}
