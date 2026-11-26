//! The Add from Cloud SQL browser and the gcloud and proxy lookups behind it. src/app/cloud_sql.rs draws it.

use super::{Refusal, Reply};
use crate::cloudsql::Found;
use crate::core::Core;
use crate::db::Msg;
use crate::paths::tools_dir;
use crate::state::cloud_sql::CloudBrowser;
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Debug, Clone)]
pub enum CloudA {
    /// Asks gcloud again, or for the first time; the demo and tests fill the cache themselves.
    Load { force: bool },
    /// Downloads the pinned proxy, or installs it with gcloud.
    Proxy { install: bool },
    /// Settings asks where gcloud is, once each time it opens.
    GcloudAsk,
    /// Settings closed: it looks again the next time it opens.
    GcloudForget,
    /// Opens the browser; `change` is for a connection form that is open, which Use then fills.
    Open { change: bool },
    Close,
    /// What the browser holds after a frame of drawing (search, picks, folds).
    Set(Box<CloudBrowser>),
}

impl Core {
    pub(super) fn cloud_act(&mut self, a: CloudA) -> Result<Reply, Refusal> {
        match a {
            CloudA::Load { force } => self.cloud_load(force),
            CloudA::Proxy { install } => self.cloud_get_proxy(install),
            CloudA::GcloudAsk => {
                if !self.demo && !self.cloud.gcloud_asked {
                    self.cloud.gcloud_asked = true;
                    self.worker.blocking(|| Msg::CloudSql { req: 0, found: Found::Gcloud(crate::cloudsql::find_gcloud("")) });
                }
            }
            CloudA::GcloudForget => (self.cloud.gcloud_asked, self.cloud.gcloud_seen) = (false, None),
            CloudA::Open { change } => {
                self.cloud_load(false);
                self.cloud_browser = Some(CloudBrowser { change, ..Default::default() });
            }
            CloudA::Close => self.cloud_browser = None,
            CloudA::Set(b) => {
                // A window closed in the same frame must not come back from a snapshot taken before.
                if self.cloud_browser.is_some() {
                    self.cloud_browser = Some(*b);
                }
            }
        }
        Ok(Reply::Done)
    }

    fn cloud_load(&mut self, force: bool) {
        if self.demo || (self.cloud.req != 0 && !force) {
            return;
        }
        self.req += 1;
        let req = self.req;
        self.cloud.reload(req);
        let (g, p) = (self.prefs.gcloud.clone(), self.prefs.cloud_sql_proxy.clone());
        self.worker.blocking(move || Msg::CloudSql { req, found: Found::Start(crate::cloudsql::start(&g, &p)) });
    }

    /// Starts the next instance lists, up to `PARALLEL` at once.
    fn cloud_pump(&mut self) {
        let req = self.cloud.req;
        let Some(g) = self.cloud.start.as_ref().and_then(|s| s.gcloud.clone()) else { return };
        while let Some(project) = self.cloud.next_project() {
            let g = g.clone();
            self.worker.blocking(move || {
                let res = crate::cloudsql::instances(&g, &project);
                Msg::CloudSql { req, found: Found::Instances { project, res } }
            });
        }
    }

    pub fn cloud_found(&mut self, req: u64, found: Found) {
        match found {
            Found::Gcloud(p) => {
                // An answer for a Settings that has closed since is dropped, so the next opening looks again.
                if self.cloud.gcloud_asked {
                    self.cloud.gcloud_seen = Some(p);
                }
            }
            // Counted whatever load it was for, since it ran until now.
            Found::Instances { project, res } => {
                self.cloud.listed(req, project, res);
                self.cloud_pump();
            }
            Found::Proxy(res) => {
                self.cloud.busy = None;
                match res {
                    Ok(p) => {
                        self.ok(format!("Cloud SQL proxy ready at {}", p.display()));
                        if let Some(s) = &mut self.cloud.start {
                            s.proxy = Some(p);
                        }
                    }
                    Err(e) => self.err(e),
                }
            }
            // An answer to a load that Refresh has replaced.
            _ if req != self.cloud.req => {}
            Found::Start(s) => {
                // A project already answered (a test feeds them first) is not asked again.
                if let Ok(ps) = &s.projects {
                    self.cloud.queue = ps.iter().filter(|p| !self.cloud.instances.contains_key(&p.id)).map(|p| p.id.clone()).collect();
                }
                self.cloud.start = Some(s);
                self.cloud.prev = None;
                self.cloud_pump();
            }
        }
    }

    fn cloud_get_proxy(&mut self, install: bool) {
        if self.demo {
            self.warn("Offline demo: nothing is downloaded.");
            return;
        }
        if self.cloud.busy.is_some() {
            return;
        }
        if install {
            let Some(g) = self.cloud.start.as_ref().and_then(|s| s.gcloud.clone()) else { return };
            self.cloud.busy = Some("Installing cloud-sql-proxy with gcloud…");
            self.worker.blocking(move || Msg::CloudSql { req: 0, found: Found::Proxy(crate::cloudsql::install_and_find(&g)) });
        } else {
            self.cloud.busy = Some("Downloading and checking cloud-sql-proxy…");
            self.worker.blocking(|| Msg::CloudSql { req: 0, found: Found::Proxy(crate::cloudsql::download_proxy(&tools_dir())) });
        }
    }
}
