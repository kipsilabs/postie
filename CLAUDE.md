# SvelteKit Best Practices Guide for NZB Upload Project

## Tooling

DON'T USE NPM use BUN instead.

## Config Change Rules

When adding or modifying configuration fields in the Rust backend (`rust/core/src/config/mod.rs`), you MUST also update the frontend:
- **API types**: Mirror the field in `frontend/src/lib/api/models.ts` (hand-maintained; it must match the JSON the Rust structs serialize)
- **Settings UI**: Update the relevant settings section components in `frontend/src/lib/components/settings/` to expose the new config fields
- **Translations**: Add i18n keys for any new user-facing labels/descriptions in `frontend/src/lib/translations/`
- **Types**: If custom frontend types reference config, update `frontend/src/lib/types.ts`

## Project Overview

This is a desktop application built with:
- **Backend**: Rust (`rust/`): `postie-core`, the `postie-web` HTTP/WebSocket server, the `postie` CLI and a Tauri v2 desktop shell
- **Frontend**: SvelteKit with TypeScript
- **Styling**: daisyUI + Tailwind CSS for consistent theming
- **State Management**: Svelte 5 runes + Svelte stores
- **Internationalization**: Custom i18n implementation with multi-language support

## Current Project Structure

```typescript
frontend/src/
├── lib/
│   ├── api/                 # API clients
│   │   ├── client.ts        # Unified API client (postie-web + Tauri dialogs)
│   │   ├── web-client.ts    # Web interface client
│   │   └── models.ts        # Shared API types (mirror the Rust structs)
│   ├── assets/
│   │   └── images/          # Static assets
│   ├── components/
│   │   ├── inputs/          # Reusable form input components
│   │   │   ├── ByteSizeInput.svelte
│   │   │   ├── DurationInput.svelte
│   │   │   └── ThrottleRateInput.svelte
│   │   ├── dashboard/       # Dashboard-specific components
│   │   │   ├── DashboardHeader.svelte
│   │   │   ├── ProgressSection.svelte
│   │   │   └── QueueStats.svelte
│   │   ├── settings/        # Settings page components
│   │   │   ├── GeneralSection.svelte
│   │   │   ├── PostingSection.svelte
│   │   │   └── ServerSection.svelte
│   │   ├── setup/           # Setup wizard components
│   │   │   ├── WelcomeStep.svelte
│   │   │   └── SetupWizard.svelte
│   │   ├── LanguageSwitcher.svelte
│   │   ├── ThemeSwitcher.svelte
│   │   └── ToastContainer.svelte
│   ├── stores/              # Global state management
│   │   ├── app.ts           # App-wide state
│   │   ├── theme.ts         # Theme management
│   │   ├── toast.ts         # Toast notifications
│   │   └── upload.ts        # Upload progress tracking
│   ├── translations/        # i18n translation files
│   │   ├── en/             # English translations
│   │   ├── es/             # Spanish translations
│   │   └── fr/             # French translations
│   ├── i18n.ts             # Internationalization utilities
│   ├── types.ts            # TypeScript type definitions
│   └── utils.ts            # Utility functions
├── routes/
│   ├── +layout.svelte      # Root layout with navigation
│   ├── +layout.ts          # Layout load functions
│   ├── +page.svelte        # Dashboard/main page
│   ├── +error.svelte       # Error page
│   ├── logs/
│   │   └── +page.svelte    # Logs viewer page
│   ├── settings/
│   │   └── +page.svelte    # Settings configuration page
│   └── setup/
│       ├── +layout.svelte  # Setup wizard layout
│       └── +page.svelte    # Setup wizard main page
├── app.html                # App shell template
└── style.css              # Global styles with daisyUI
```

### File Naming Conventions

- Components: `PascalCase.svelte` (e.g., `UserProfile.svelte`)
- Routes: `+page.svelte`, `+layout.svelte`, `+error.svelte`
- Utilities: `camelCase.ts` (e.g., `formatDate.ts`)
- Types: `PascalCase.ts` or `types.ts`

## Svelte 5 Runes Best Practices

### State Management

```svelte
<script lang="ts">
// ✅ Use $state for reactive values
let count = $state(0);
let user = $state<User | null>(null);

// ✅ Use $derived for computed values
let doubled = $derived(count * 2);
let userDisplayName = $derived(user?.name ?? 'Anonymous');

// ✅ Use $effect for side effects
$effect(() => {
  document.title = `Count: ${count}`;
});

// ✅ Use $effect.pre for pre-DOM update effects
$effect.pre(() => {
  console.log('Before DOM update');
});

// ✅ Cleanup effects
$effect(() => {
  const interval = setInterval(() => {
    count++;
  }, 1000);

  return () => clearInterval(interval);
});
</script>
```

### Component Props (Svelte 5)

```svelte
<script lang="ts">
interface Props {
  title: string;
  count?: number;
  items: string[];
  onSelect?: (item: string) => void;
}

// ✅ Destructure props with defaults
let { title, count = 0, items, onSelect }: Props = $props();

// ✅ Use $bindable for two-way binding
let { value = $bindable() }: { value: string } = $props();
</script>
```

### Event Handling

```svelte
<script lang="ts">
// ✅ Use event handlers properly
function handleClick(event: MouseEvent) {
  console.log('Clicked:', event.target);
}

// ✅ Pass data with event handlers
function handleItemClick(item: string) {
  return (event: MouseEvent) => {
    onSelect?.(item);
  };
}

// ✅ Early return pattern - avoid unnecessary else clauses
function processValue(value: number, type: string): number {
  if (type === "percentage") {
    return value * 100;
  }
  if (type === "decimal") {
    return value / 100;
  }
  return value;
}
</script>

<button onclick={handleClick}>Click me</button>
<button onclick={() => handleItemClick('item1')}>Select Item 1</button>
```

## Component Patterns

### Base Component Structure

```svelte
<script lang="ts">
// Imports
import { onMount } from 'svelte';
import type { ComponentEvents } from 'svelte';

// Types
interface Props {
  // Define props
}

// Props
let { /* destructure props */ }: Props = $props();

// State
let localState = $state(initialValue);

// Derived state
let computedValue = $derived(/* computation */);

// Effects
$effect(() => {
  // Side effects
});

// Functions
function handleAction() {
  // Handle user actions
}

// Lifecycle (if needed)
onMount(() => {
  // Component mounted
});
</script>

<!-- Template -->
<div>
  <!-- Component content -->
</div>

<style>
  /* Component styles */
</style>
```

### Input Component Patterns (Following Project Structure)

```svelte
<!-- ByteSizeInput.svelte -->
<script lang="ts">
import { t } from "$lib/i18n";

interface Props {
  value: number;
  label: string;
  description?: string;
  presets?: Array<{label: string, value: number}>;
  minValue?: number;
  maxValue?: number;
  id: string;
}

let { 
  value = $bindable(), 
  label, 
  description, 
  presets = [], 
  minValue, 
  maxValue,
  id 
}: Props = $props();

let displayValue = $state(Math.round(value / 1000)); // Convert to KB for display

function updateValue() {
  if (displayValue !== undefined && !Number.isNaN(displayValue)) {
    value = displayValue * 1000; // Convert back to bytes
  }
}

function usePreset(presetValue: number) {
  value = presetValue;
  displayValue = Math.round(presetValue / 1000);
}
</script>

<div>
  <label for={id} class="label">
    <span class="label-text">{label}</span>
  </label>
  
  <div class="join w-full">
    <input
      {id}
      type="number"
      class="input input-bordered join-item flex-1"
      bind:value={displayValue}
      onchange={updateValue}
      min={minValue ? Math.round(minValue / 1000) : undefined}
      max={maxValue ? Math.round(maxValue / 1000) : undefined}
    />
    <span class="btn btn-ghost join-item">KB</span>
  </div>
  
  {#if presets.length > 0}
    <div class="flex flex-wrap gap-2 mt-2">
      {#each presets as preset}
        <button
          type="button"
          class="btn btn-xs btn-outline"
          onclick={() => usePreset(preset.value)}
        >
          {preset.label}
        </button>
      {/each}
    </div>
  {/if}
  
  {#if description}
    <p class="text-sm text-base-content/70 mt-1">{description}</p>
  {/if}
</div>
```

```svelte
<!-- DurationInput.svelte -->
<script lang="ts">
import { t } from "$lib/i18n";

interface Props {
  value: string;
  label: string;
  description?: string;
  presets?: Array<{label: string, value: number, unit: string}>;
  id: string;
}

let { 
  value = $bindable(), 
  label, 
  description, 
  presets = [], 
  id 
}: Props = $props();

function usePreset(presetValue: number, unit: string) {
  value = `${presetValue}${unit}`;
}
</script>

<div>
  <label for={id} class="label">
    <span class="label-text">{label}</span>
  </label>
  
  <input
    {id}
    type="text"
    class="input input-bordered w-full"
    bind:value
    placeholder="5s, 1m, 1h"
  />
  
  {#if presets.length > 0}
    <div class="flex flex-wrap gap-2 mt-2">
      {#each presets as preset}
        <button
          type="button"
          class="btn btn-xs btn-outline"
          onclick={() => usePreset(preset.value, preset.unit)}
        >
          {preset.label}
        </button>
      {/each}
    </div>
  {/if}
  
  {#if description}
    <p class="text-sm text-base-content/70 mt-1">{description}</p>
  {/if}
</div>
```

### Store Integration (Project-Specific)

```svelte
<script lang="ts">
import { toastStore } from "$lib/stores/toast";
import { advancedMode } from "$lib/stores/app";
import { currentTheme } from "$lib/stores/theme";
import { uploadProgress } from "$lib/stores/upload";

// ✅ Use project stores reactively
let isAdvanced = $derived($advancedMode);
let theme = $derived($currentTheme);
let progress = $derived($uploadProgress);

// ✅ Update stores with project methods
function showSuccess(message: string) {
  toastStore.success("Operation completed", message);
}

function toggleAdvancedMode() {
  advancedMode.update(current => !current);
}
</script>

{#if $isAdvanced}
  <!-- Advanced settings -->
{/if}
```

```typescript
// stores/app.ts - Project store pattern
import { writable } from 'svelte/store';

export const advancedMode = writable<boolean>(false);
export const isSetupComplete = writable<boolean>(false);

// Custom store with methods
function createAppStore() {
  const { subscribe, set, update } = writable({
    isLoading: false,
    currentStep: 0,
    totalSteps: 3
  });

  return {
    subscribe,
    setLoading: (loading: boolean) => update(state => ({ ...state, isLoading: loading })),
    nextStep: () => update(state => ({ 
      ...state, 
      currentStep: Math.min(state.currentStep + 1, state.totalSteps) 
    })),
    reset: () => set({ isLoading: false, currentStep: 0, totalSteps: 3 })
  };
}

export const appStore = createAppStore();
```

## Routing Best Practices (Current Project Structure)

### Current Route Organization

```typescript
routes/
├── +layout.svelte           # Root layout with navigation
├── +layout.ts              # Load user preferences and theme
├── +page.svelte            # Dashboard (main page)
├── +error.svelte           # Error handling page
├── logs/
│   └── +page.svelte        # Logs viewer with real-time updates
├── settings/
│   └── +page.svelte        # Settings page with multiple sections
└── setup/
    ├── +layout.svelte      # Setup wizard layout
    └── +page.svelte        # Setup wizard main component
```

### Load Functions

```typescript
// +layout.ts - Root layout load function
import type { LayoutLoad } from './$types';
import { browser } from '$app/environment';
import apiClient from '$lib/api/client';

export const load: LayoutLoad = async () => {
  // ✅ Browser-only data loading (the API is served by postie-web)
  if (!browser) {
    return { config: null };
  }
  try {
    return { config: await apiClient.getConfig() };
  } catch {
    return { config: null };
  }
};
```

## State Management

### Svelte Stores

```typescript
// stores/user.ts
import { writable, derived } from 'svelte/store';

interface User {
  id: string;
  name: string;
  email: string;
}

// ✅ Create typed stores
export const user = writable<User | null>(null);
export const isLoggedIn = derived(user, $user => !!$user);

// ✅ Custom store with methods
function createUserStore() {
  const { subscribe, set, update } = writable<User | null>(null);

  return {
    subscribe,
    login: (userData: User) => set(userData),
    logout: () => set(null),
    updateProfile: (updates: Partial<User>) => 
      update(user => user ? { ...user, ...updates } : null)
  };
}

export const userStore = createUserStore();
```

### Context API

```svelte
<!-- Parent.svelte -->
<script lang="ts">
import { setContext } from 'svelte';

interface ThemeContext {
  theme: string;
  toggleTheme: () => void;
}

let theme = $state('light');

function toggleTheme() {
  theme = theme === 'light' ? 'dark' : 'light';
}

// ✅ Set context
setContext<ThemeContext>('theme', {
  get theme() { return theme; },
  toggleTheme
});
</script>

<slot />
```

```svelte
<!-- Child.svelte -->
<script lang="ts">
import { getContext } from 'svelte';

// ✅ Get context
const themeContext = getContext<ThemeContext>('theme');
</script>

<button onclick={themeContext.toggleTheme}>
  Current theme: {themeContext.theme}
</button>
```

## Forms and Validation (Project Patterns)

### Settings Form Handling

```svelte
<script lang="ts">
import apiClient from "$lib/api/client";
import ByteSizeInput from "$lib/components/inputs/ByteSizeInput.svelte";
import DurationInput from "$lib/components/inputs/DurationInput.svelte";
import { t } from "$lib/i18n";
import { toastStore } from "$lib/stores/toast";
import type { config as configType } from "$lib/api/models";

interface ComponentProps {
  config: configType.ConfigData;
}

const { config }: ComponentProps = $props();

let saving = $state(false);

// Create reactive local state for form inputs (Svelte 5 pattern)
let maxRetries = $state(config.posting.max_retries || 3);
let retryDelay = $state(config.posting.retry_delay || "5s");
let articleSizeInBytes = $state(config.posting.article_size_in_bytes || 750000);

// Sync local state back to config when values change
$effect(() => {
  config.posting.max_retries = maxRetries;
});

$effect(() => {
  config.posting.retry_delay = retryDelay;
});

$effect(() => {
  config.posting.article_size_in_bytes = articleSizeInBytes;
});

async function saveSettings() {
  try {
    saving = true;

    // Get current config to avoid conflicts
    const currentConfig = await apiClient.getConfig();

    // Update only the specific section
    currentConfig.posting = {
      ...config.posting,
      max_retries: Number.parseInt(config.posting.max_retries) || 3,
      article_size_in_bytes: Number.parseInt(config.posting.article_size_in_bytes) || 750000,
      retry_delay: config.posting.retry_delay || "5s"
    };

    await apiClient.saveConfig(currentConfig);

    toastStore.success(
      $t("settings.posting.saved_success"),
      $t("settings.posting.saved_success_description")
    );
  } catch (error) {
    console.error("Failed to save settings:", error);
    toastStore.error($t("common.messages.error_saving"), String(error));
  } finally {
    saving = false;
  }
}
</script>

<div class="card bg-base-100 shadow-sm">
  <div class="card-body space-y-6">
    <div class="grid grid-cols-1 md:grid-cols-2 gap-6">
      <div>
        <label for="max-retries" class="label">
          <span class="label-text">{$t('settings.posting.max_retries')}</span>
        </label>
        <input
          id="max-retries"
          type="number"
          class="input input-bordered w-full"
          bind:value={maxRetries}
          min="0"
          max="10"
        />
      </div>

      <DurationInput
        bind:value={retryDelay}
        label={$t('settings.posting.retry_delay')}
        description={$t('settings.posting.retry_delay_description')}
        id="retry-delay"
      />

      <ByteSizeInput
        bind:value={articleSizeInBytes}
        label={$t('settings.posting.article_size')}
        description={$t('settings.posting.article_size_description')}
        minValue={1000}
        maxValue={10000000}
        id="article-size"
      />
    </div>

    <div class="pt-4 border-t border-base-300">
      <button
        class="btn btn-success"
        onclick={saveSettings}
        disabled={saving}
      >
        {saving ? $t('common.common.saving') : $t('settings.posting.save_button')}
      </button>
    </div>
  </div>
</div>
```

### Internationalization Integration

```svelte
<script lang="ts">
import { t } from "$lib/i18n";

// ✅ Using project's i18n system
let validationMessage = $derived($t('settings.validation.required_field'));
let successMessage = $derived($t('settings.posting.saved_success'));

// ✅ Dynamic translations with parameters
let itemCount = $state(5);
let countMessage = $derived($t('dashboard.items_count', { count: itemCount }));
</script>

<div class="alert alert-success">
  <span>{successMessage}</span>
</div>

<p>{countMessage}</p>
```

```typescript
// lib/translations/en/settings.json
{
  "posting": {
    "title": "Posting Configuration",
    "max_retries": "Maximum Retries",
    "retry_delay": "Retry Delay",
    "saved_success": "Settings saved successfully",
    "saved_success_description": "Your posting configuration has been updated"
  },
  "validation": {
    "required_field": "This field is required",
    "invalid_email": "Please enter a valid email address"
  }
}
```

## Performance Optimization

### Code Splitting

```svelte
<script lang="ts">
// ✅ Dynamic imports for heavy components
let HeavyComponent: typeof import('$lib/HeavyComponent.svelte').default;

async function loadHeavyComponent() {
  const module = await import('$lib/HeavyComponent.svelte');
  HeavyComponent = module.default;
}

let showHeavy = $state(false);

// ✅ Load on demand
$effect(() => {
  if (showHeavy && !HeavyComponent) {
    loadHeavyComponent();
  }
});
</script>

{#if showHeavy}
  {#if HeavyComponent}
    <svelte:component this={HeavyComponent} />
  {:else}
    <div>Loading...</div>
  {/if}
{/if}
```

### Efficient Updates

```svelte
<script lang="ts">
// ✅ Use keys for list items
let items = $state<Item[]>([]);

// ✅ Avoid expensive computations in templates
let sortedItems = $derived(
  items.toSorted((a, b) => a.name.localeCompare(b.name))
);
</script>

{#each sortedItems as item (item.id)}
  <ItemComponent {item} />
{/each}
```

## Testing Best Practices

### Component Testing

```typescript
// Component.test.ts
import { render, screen } from '@testing-library/svelte';
import { vi } from 'vitest';
import Component from './Component.svelte';

describe('Component', () => {
  it('renders correctly', () => {
    render(Component, { 
      props: { title: 'Test Title' } 
    });
    
    expect(screen.getByText('Test Title')).toBeInTheDocument();
  });
  
  it('handles user interaction', async () => {
    const onClickMock = vi.fn();
    
    render(Component, { 
      props: { onClick: onClickMock } 
    });
    
    await screen.getByRole('button').click();
    
    expect(onClickMock).toHaveBeenCalled();
  });
});
```

### API Route Testing

```typescript
// +server.test.ts
import { describe, it, expect } from 'vitest';
import { GET } from './+server.js';

describe('/api/users', () => {
  it('returns users list', async () => {
    const request = new Request('http://localhost/api/users');
    const response = await GET({ request });
    
    expect(response.status).toBe(200);
    
    const data = await response.json();
    expect(Array.isArray(data)).toBe(true);
  });
});
```

## Type Safety

### Shared API Types (Priority #1)

**ALWAYS use the shared API types**. `$lib/api/models.ts` mirrors the JSON of the Rust backend structs (config, queue, status) and must be kept in sync with them.

```typescript
// ✅ ALWAYS use the shared API types
import type { config } from "$lib/api/models";

interface Props {
  // ✅ Use the shared ServerConfig type
  servers?: config.ServerConfig[];
  configData?: config.ConfigData;
}

// ✅ Create instances using the model constructors
function addServer(): void {
  const newServer = new config.ServerConfig();
  // All fields are properly typed and have correct defaults
  servers = [...servers, newServer];
}
```

```typescript
// ❌ DON'T create custom interfaces that duplicate the shared API types
interface ServerConfig {
  host: string;
  port: number;
  // ... duplicating what's already in models.ts
}

// ✅ DO use the shared types directly
import type { config } from "$lib/api/models";
type ServerConfig = config.ServerConfig;
```

### Available Shared API Types

The main types available in `$lib/api/models` include:
- `config.ConfigData` - Main application configuration
- `config.ServerConfig` - NNTP server configuration
- `config.WatcherConfig` - File watcher configuration
- `config.PostingConfig` - Posting/upload configuration
- `config.ScheduleConfig` - Scheduling configuration

### Comprehensive TypeScript Setup

```typescript
// app.d.ts
declare global {
  namespace App {
    interface Error {
      code?: string;
    }
    
    interface Locals {
      user?: User;
    }
    
    interface PageData {
      user?: User;
    }
    
    interface Platform {}
  }
}

export {};
```

```typescript
// lib/types.ts - Only for frontend-only types not in models.ts
export interface User {
  id: string;
  name: string;
  email: string;
  createdAt: Date;
}

export interface ApiResponse<T> {
  data: T;
  error?: string;
  success: boolean;
}

// ✅ Utility types
export type CreateUser = Omit<User, 'id' | 'createdAt'>;
export type UpdateUser = Partial<CreateUser>;
```

## Security Best Practices

### CSRF Protection

```typescript
// hooks.server.ts
import { csrfProtection } from '$lib/server/csrf';

export const handle = csrfProtection({
  secret: process.env.CSRF_SECRET
});
```

### Input Sanitization

```typescript
// lib/utils/sanitize.ts
import DOMPurify from 'isomorphic-dompurify';

export function sanitizeHtml(html: string): string {
  return DOMPurify.sanitize(html, {
    ALLOWED_TAGS: ['b', 'i', 'em', 'strong', 'p'],
    ALLOWED_ATTR: []
  });
}
```

### Environment Variables

```typescript
// lib/config.ts
import { env } from '$env/dynamic/private';
import { PUBLIC_API_URL } from '$env/static/public';

export const config = {
  apiUrl: PUBLIC_API_URL,
  dbUrl: env.DATABASE_URL,
  jwtSecret: env.JWT_SECRET
};
```

## Accessibility

### Semantic HTML and ARIA

```svelte
<script lang="ts">
let isOpen = $state(false);
let menuId = 'menu-' + Math.random().toString(36).substr(2, 9);
</script>

<nav>
  <button
    type="button"
    aria-expanded={isOpen}
    aria-controls={menuId}
    onclick={() => isOpen = !isOpen}
  >
    Menu
  </button>
  
  <ul
    id={menuId}
    role="menu"
    hidden={!isOpen}
    aria-hidden={!isOpen}
  >
    <li role="menuitem">
      <a href="/home">Home</a>
    </li>
    <li role="menuitem">
      <a href="/about">About</a>
    </li>
  </ul>
</nav>
```

### Focus Management

```svelte
<script lang="ts">
import { tick } from 'svelte';

let dialogElement = $state<HTMLDialogElement>();
let isOpen = $state(false);

async function openDialog() {
  isOpen = true;
  await tick();
  dialogElement?.showModal();
  
  // Focus first focusable element
  const firstFocusable = dialogElement?.querySelector(
    'button, [href], input, select, textarea, [tabindex]:not([tabindex="-1"])'
  ) as HTMLElement;
  firstFocusable?.focus();
}

function closeDialog() {
  isOpen = false;
  dialogElement?.close();
}

function handleKeydown(event: KeyboardEvent) {
  if (event.key === 'Escape') {
    closeDialog();
  }
}
</script>

<dialog
  bind:this={dialogElement}
  onkeydown={handleKeydown}
  onclose={closeDialog}
>
  <div role="document">
    <h2>Dialog Title</h2>
    <button onclick={closeDialog}>Close</button>
  </div>
</dialog>
```

## Styling with daisyUI (Project Standards)

### Semantic Color System

```svelte
<!-- ✅ Use daisyUI semantic colors for consistency -->
<div class="card bg-base-100 shadow-sm">
  <div class="card-body">
    <h2 class="card-title text-base-content">Settings</h2>
    <p class="text-base-content/70">Configure your application</p>
    
    <div class="card-actions justify-end">
      <button class="btn btn-primary">Save</button>
      <button class="btn btn-ghost">Cancel</button>
    </div>
  </div>
</div>

<!-- ✅ Modern UI enhancements -->
<nav class="navbar bg-base-100/95 backdrop-blur-md shadow-lg border-b border-base-300/50 sticky top-0 z-50">
  <div class="navbar-brand">
    <img src="/logo.png" alt="Logo" class="w-8 h-8" />
  </div>
</nav>
```

### Component Styling Patterns

```css
/* Global styles in style.css */
@import 'tailwindcss/base';
@import 'tailwindcss/components';
@import 'tailwindcss/utilities';

/* Custom animations for modern UI */
@keyframes fade-in {
  from { opacity: 0; transform: translateY(10px); }
  to { opacity: 1; transform: translateY(0); }
}

@keyframes glow {
  0%, 100% { box-shadow: 0 0 5px rgba(59, 130, 246, 0.5); }
  50% { box-shadow: 0 0 20px rgba(59, 130, 246, 0.8); }
}

.animate-fade-in {
  animation: fade-in 0.5s ease-out;
}

.glow-effect {
  animation: glow 2s ease-in-out infinite alternate;
}
```

## Build and Deployment

The frontend is a static SvelteKit build (`frontend/build`). `postie-web` serves it from a `frontend/`
directory next to its binary (or `POSTIE_FRONTEND_DIR`); the Tauri desktop app bundles it as a resource
and loads it from its in-process loopback server.

### Development Commands

```bash
# Frontend
cd frontend
bun run dev              # SvelteKit dev server
bun run build            # production build into frontend/build
bun run check            # svelte-check
bun test                 # unit tests

# Backend (from the repo root)
make dev                 # postie-web on http://localhost:8080
make desktop-dev         # Tauri desktop app
make build               # release CLI + postie-web
make lint test           # rustfmt, clippy, svelte-check, cargo + bun tests
```

## Common Anti-Patterns to Avoid

### ❌ Project-Specific Anti-Patterns

```svelte
<script lang="ts">
// ❌ Direct prop mutation (breaks Svelte 5 reactivity)
let { config } = $props();
config.posting.max_retries = 5; // Don't mutate props directly

// ❌ Missing error handling
async function saveConfig() {
  await apiClient.saveConfig(config); // No error handling!
}

// ❌ Hardcoded text instead of i18n
<h1>Settings</h1> // Should use $t('settings.title')

// ❌ Inconsistent styling
<div class="bg-white text-gray-900"> // Use daisyUI semantic colors

// ❌ Missing reactive state sync
let maxRetries = config.posting.max_retries;
// Missing $effect to sync back to config

// ❌ Using if-else return pattern
function getValue(unit: string): number {
  if (unit === "GB") {
    return value / 1024;
  } else {
    return value; // Unnecessary else clause
  }
}
</script>
```

### ✅ Project-Specific Best Practices

```svelte
<script lang="ts">
import { t } from "$lib/i18n";
import { toastStore } from "$lib/stores/toast";
import apiClient from "$lib/api/client";

// ✅ Create local reactive state
let { config } = $props();
let maxRetries = $state(config.posting.max_retries || 3);

// ✅ Sync local state back to config
$effect(() => {
  config.posting.max_retries = maxRetries;
});

// ✅ Proper API error handling
async function saveConfig() {
  try {
    await apiClient.saveConfig(config);
    toastStore.success($t('settings.saved_success'));
  } catch (error) {
    console.error('Save failed:', error);
    toastStore.error($t('common.error_saving'), String(error));
  }
}

// ✅ Early return pattern (no unnecessary else clauses)
function getValue(unit: string): number {
  if (unit === "GB") {
    return value / 1024;
  }
  return value;
}
</script>

<!-- ✅ Use i18n and semantic colors -->
<div class="card bg-base-100 shadow-sm">
  <h1 class="text-base-content">{$t('settings.title')}</h1>
  
  <input
    type="number"
    class="input input-bordered w-full"
    bind:value={maxRetries}
  />
  
  <button class="btn btn-primary" onclick={saveConfig}>
    {$t('common.save')}
  </button>
</div>
```

## Development Workflow Summary

### Key Project Patterns

1. **State Management**: Use Svelte 5 runes for local state, stores for global state
2. **Form Handling**: Create local reactive state, sync with $effect, save via apiClient
3. **Styling**: Use daisyUI semantic colors and component classes consistently
4. **Internationalization**: Always use $t() for user-facing text
5. **Error Handling**: Always wrap API calls in try-catch with toast notifications
6. **Component Structure**: Follow the input component pattern with presets and descriptions

### Architecture Decisions

- **Desktop Integration**: Tauri v2 shell around the same web UI, served by an in-process postie-web
- **State Management**: Svelte 5 runes + stores for different scopes
- **Styling**: daisyUI + Tailwind for consistent, theme-aware design
- **Internationalization**: Custom i18n system with multi-language support
- **API Communication**: Abstracted client over the postie-web HTTP/WebSocket API
- **Build Setup**: SvelteKit with static adapter for desktop deployment

### Maintenance Guidelines

- Keep components small and focused on single responsibilities
- Use TypeScript interfaces for all props and data structures
- Follow the established input component patterns for consistency
- Always handle loading and error states in async operations
- Test both the desktop (Tauri) and web modes when applicable
- Use semantic commit messages and update CLAUDE.md for significant patterns

This guide ensures consistency across the NZB Upload Project while following community best practices for SvelteKit development.