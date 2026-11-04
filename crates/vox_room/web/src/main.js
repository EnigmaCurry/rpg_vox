import { mount } from 'svelte';
import './app.css';
import { wearTheme } from './lib/theme.svelte.js';
import App from './App.svelte';

// The look is already on the page -- `index.html` puts it there before
// the first paint. This is the second half of that: it reconciles what
// the attribute says with what the app knows, so a stored theme that no
// longer exists becomes the default here rather than nowhere.
wearTheme();

mount(App, { target: document.getElementById('app') });
