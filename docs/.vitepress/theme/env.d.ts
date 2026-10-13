// Type shims for the theme: single-file components and the stylesheet
// side-effect import, which the type checker cannot resolve on its own.
declare module "*.vue" {
  import type { DefineComponent } from "vue";
  const component: DefineComponent<Record<string, never>, Record<string, never>, unknown>;
  export default component;
}

declare module "*.css";
