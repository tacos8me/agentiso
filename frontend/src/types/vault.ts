export interface VaultFrontmatter {
  [key: string]: string | string[] | number | boolean | undefined;
  status?: string;
  tags?: string[];
  priority?: string;
  created?: string;
  updated?: string;
}

export interface VaultNote {
  path: string;
  name: string;
  content: string;
  frontmatter: VaultFrontmatter;
  backlinks: string[];
  outlinks: string[];
  tags: string[];
  modified_at: string;
}

export interface VaultFolder {
  name: string;
  path: string;
  children: VaultTreeNode[];
}

export type VaultTreeNode =
  | { type: "folder"; folder: VaultFolder }
  | { type: "note"; note: VaultNote };

export interface VaultGraphNode {
  id: string;
  name: string;
  folder: string;
  tags: string[];
  val: number;
}

export interface VaultGraphLink {
  source: string;
  target: string;
}

export interface VaultGraphData {
  nodes: VaultGraphNode[];
  links: VaultGraphLink[];
}

export interface VaultSearchResult {
  path: string;
  name: string;
  line: number;
  context: string;
  matchStart: number;
  matchEnd: number;
}
