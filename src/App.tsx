import { Shell } from "./components/Shell";
import { AppProvider } from "./state/AppStore";
import { TerminalProvider } from "./state/TerminalStore";

export default function App() {
  return (
    <AppProvider>
      <TerminalProvider>
        <Shell />
      </TerminalProvider>
    </AppProvider>
  );
}
