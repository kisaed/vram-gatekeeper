/** Entry point: mounts the app and loads the global styles. */
import { mount } from 'svelte';
import './app.css';
import App from './App.svelte';

const target = document.getElementById('app');
if (!target) throw new Error('missing #app element');

export default mount(App, { target });
