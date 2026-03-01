import { BrowserRouter, Routes, Route } from "react-router-dom";
import { LocaleProvider } from "./hooks/useLocale";
import Layout from "./components/Layout";
import Dashboard from "./pages/Dashboard";
import AccountsPage from "./pages/AccountsPage";
import ProxyPage from "./pages/ProxyPage";
import MonitorPage from "./pages/MonitorPage";
import SettingsPage from "./pages/SettingsPage";

function App() {
  return (
    <LocaleProvider>
      <BrowserRouter>
        <Routes>
          <Route element={<Layout />}>
            <Route path="/" element={<Dashboard />} />
            <Route path="/accounts" element={<AccountsPage />} />
            <Route path="/proxy" element={<ProxyPage />} />
            <Route path="/monitor" element={<MonitorPage />} />
            <Route path="/settings" element={<SettingsPage />} />
          </Route>
        </Routes>
      </BrowserRouter>
    </LocaleProvider>
  );
}

export default App;
