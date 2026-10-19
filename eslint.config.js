/* ESLint flat config：Vue 3 + TypeScript。
 * 只启用能抓真实问题的规则集（vue essential + typescript recommended），
 * 风格类规则交给编辑器/Prettier 生态，避免与现有手写风格大面积冲突。 */
import pluginVue from "eslint-plugin-vue";
import tseslint from "typescript-eslint";

export default tseslint.config(
  {
    ignores: [
      "dist/**",
      "node_modules/**",
      "src-tauri/**", // Rust 侧由 cargo fmt / clippy 负责
      "src/vite-env.d.ts",
    ],
  },
  ...tseslint.configs.recommended,
  ...pluginVue.configs["flat/essential"],
  {
    files: ["**/*.vue"],
    languageOptions: {
      parserOptions: {
        parser: tseslint.parser,
        extraFileExtensions: [".vue"],
        sourceType: "module",
      },
    },
  },
  {
    rules: {
      // App 为根组件单词名是官方脚手架约定
      "vue/multi-word-component-names": ["error", { ignores: ["App"] }],
      // markdown.ts 预览先整体转义再生成白名单标签，v-html 属于受控内容
      // （链接只生成 data-url，由组件拦截点击走 opener）
      "vue/no-v-html": "off",
    },
  },
);
