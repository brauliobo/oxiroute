#!/usr/bin/env bash
# Builds the combined GitHub Pages directory containing the static website plus
# hosted multi-arch APT (Debian/Ubuntu) and RPM (Fedora/RHEL/openSUSE & Amazon Linux 2023)
# repositories populated from recent published GitHub Releases.
set -euo pipefail

repo_dir=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/../.." && pwd)
out_dir=${1:-"${repo_dir}/site"}
gh_repo=${GH_REPO:-brauliobo/oxiroute}
input_tag=${INPUT_TAG:-}
max_releases=${MAX_RELEASES:-3}

repo_owner=${gh_repo%%/*}
repo_name=${gh_repo#*/}
base_url="https://${repo_owner}.github.io/${repo_name}"

rm -rf -- "${out_dir}"
mkdir -p -- \
  "${out_dir}" \
  "${out_dir}/apt/pool" \
  "${out_dir}/rpm/packages" \
  "${out_dir}/rpm/amzn2023/packages" \
  "${out_dir}/repos"

# Preserve the existing documentation website at the root of GitHub Pages.
cp -a "${repo_dir}/website/." "${out_dir}/"

mapfile -t recent_tags < <(
  gh release list --repo "${gh_repo}" \
    --exclude-drafts \
    --exclude-pre-releases \
    --limit "${max_releases}" \
    --json tagName \
    --jq '.[].tagName'
)

tags=()
if [[ -n "${input_tag}" ]]; then
  tags+=("${input_tag}")
fi
for t in "${recent_tags[@]}"; do
  skip=false
  for existing in "${tags[@]}"; do
    if [[ "${existing}" == "${t}" ]]; then
      skip=true
      break
    fi
  done
  if [[ "${skip}" == false && ${#tags[@]} -lt ${max_releases} ]]; then
    tags+=("${t}")
  fi
done

if [[ ${#tags[@]} -eq 0 ]]; then
  printf 'error: no published GitHub Releases found in %s\n' "${gh_repo}" >&2
  exit 1
fi

latest_tag="${tags[0]}"
printf 'Selected release tags (newest first): %s\n' "${tags[*]}"

# Download oldest-to-newest so newer releases win on any identical filename.
for (( idx=${#tags[@]}-1; idx>=0; idx-- )); do
  tag="${tags[$idx]}"
  printf 'Fetching .deb and .rpm assets from release %s...\n' "${tag}"
  gh release download "${tag}" \
    --repo "${gh_repo}" \
    --pattern '*.deb' \
    --dir "${out_dir}/apt/pool" \
    --clobber || true
  gh release download "${tag}" \
    --repo "${gh_repo}" \
    --pattern '*.el9.*.rpm' \
    --dir "${out_dir}/rpm/packages" \
    --clobber || true
  gh release download "${tag}" \
    --repo "${gh_repo}" \
    --pattern '*.amzn2023.*.rpm' \
    --dir "${out_dir}/rpm/amzn2023/packages" \
    --clobber || true
done

deb_count=$(find "${out_dir}/apt/pool" -maxdepth 1 -type f -name '*.deb' | wc -l)
el9_rpm_count=$(find "${out_dir}/rpm/packages" -maxdepth 1 -type f -name '*.rpm' | wc -l)
amzn_rpm_count=$(find "${out_dir}/rpm/amzn2023/packages" -maxdepth 1 -type f -name '*.rpm' | wc -l)
printf 'Downloaded %d .deb, %d el9 .rpm, and %d amzn2023 .rpm package(s).\n' \
  "${deb_count}" "${el9_rpm_count}" "${amzn_rpm_count}"

if [[ "${deb_count}" -eq 0 && "${el9_rpm_count}" -eq 0 && "${amzn_rpm_count}" -eq 0 ]]; then
  printf 'error: no .deb or .rpm assets were downloaded from releases: %s\n' "${tags[*]}" >&2
  exit 1
fi

# Build static multi-arch APT repository metadata (amd64 + arm64).
pushd "${out_dir}/apt" >/dev/null
dpkg-scanpackages --multiversion pool > Packages
gzip -k -9 -f Packages
apt-ftparchive \
  -o APT::FTPArchive::Release::Origin="oxiroute" \
  -o APT::FTPArchive::Release::Label="oxiroute" \
  -o APT::FTPArchive::Release::Suite="stable" \
  -o APT::FTPArchive::Release::Codename="stable" \
  -o APT::FTPArchive::Release::Architectures="amd64 arm64" \
  -o APT::FTPArchive::Release::Description="OxiRoute APT repository (Debian 12/13, Ubuntu 22.04/24.04; amd64 & arm64)" \
  release . > /tmp/oxiroute-apt-Release
mv /tmp/oxiroute-apt-Release Release
popd >/dev/null

# Build static multi-arch RPM/DNF/Zypper repository metadata (Fedora/RHEL/Alma/Rocky/openSUSE).
createrepo_c --excludes 'amzn2023/*' "${out_dir}/rpm"
cat > "${out_dir}/rpm/oxiroute.repo" <<EOF
[oxiroute]
name=OxiRoute - Rust Proxy & Control Plane (Fedora / RHEL / AlmaLinux / Rocky Linux)
baseurl=${base_url}/rpm
enabled=1
gpgcheck=0
repo_gpgcheck=0
EOF

# Build static multi-arch Amazon Linux 2023 RPM/DNF repository metadata.
createrepo_c "${out_dir}/rpm/amzn2023"
cat > "${out_dir}/rpm/amzn2023/oxiroute.repo" <<EOF
[oxiroute]
name=OxiRoute - Rust Proxy & Control Plane (Amazon Linux 2023)
baseurl=${base_url}/rpm/amzn2023
enabled=1
gpgcheck=0
repo_gpgcheck=0
EOF

# Generate repository landing page at /repos/index.html.
updated_utc=$(date -u '+%Y-%m-%d %H:%M UTC')

deb_list=""
while IFS= read -r f; do
  fname=$(basename -- "${f}")
  deb_list+="          <li><a href=\"../apt/pool/${fname}\">${fname}</a></li>"$'\n'
done < <(find "${out_dir}/apt/pool" -maxdepth 1 -type f -name '*.deb' | sort -V)

el9_list=""
while IFS= read -r f; do
  fname=$(basename -- "${f}")
  el9_list+="          <li><a href=\"../rpm/packages/${fname}\">${fname}</a></li>"$'\n'
done < <(find "${out_dir}/rpm/packages" -maxdepth 1 -type f -name '*.rpm' | sort -V)

amzn_list=""
while IFS= read -r f; do
  fname=$(basename -- "${f}")
  amzn_list+="          <li><a href=\"../rpm/amzn2023/packages/${fname}\">${fname}</a></li>"$'\n'
done < <(find "${out_dir}/rpm/amzn2023/packages" -maxdepth 1 -type f -name '*.rpm' | sort -V)

cat > "${out_dir}/repos/index.html" <<EOF
<!DOCTYPE html>
<html lang="en">
<head>
  <meta charset="utf-8">
  <meta name="viewport" content="width=device-width, initial-scale=1">
  <title>OxiRoute — Linux Package Repositories (APT &amp; RPM)</title>
  <style>
    :root {
      color-scheme: dark;
      --bg: #0D1017;
      --card: #141925;
      --border: #30394D;
      --fg: #E6EDF3;
      --muted: #9AA6BC;
      --accent: #FF7666;
      --cyan: #38BDF8;
      --code-bg: #090C12;
    }
    * { box-sizing: border-box; }
    body {
      margin: 0;
      padding: 2rem 1.25rem 3.5rem;
      background: var(--bg);
      color: var(--fg);
      font-family: system-ui, -apple-system, BlinkMacSystemFont, "Segoe UI", Roboto, sans-serif;
      line-height: 1.55;
    }
    main { max-width: 860px; margin: 0 auto; }
    h1 { margin: 0 0 0.4rem; font-size: 1.85rem; }
    h1 span { color: var(--accent); }
    h2 { margin: 0 0 0.75rem; font-size: 1.2rem; color: var(--cyan); }
    h3 { margin: 1.1rem 0 0.4rem; font-size: 0.98rem; color: var(--fg); }
    p { color: var(--muted); margin: 0 0 1rem; }
    a { color: var(--cyan); text-decoration: none; }
    a:hover { text-decoration: underline; }
    .meta { font-size: 0.9rem; margin-bottom: 1.5rem; }
    .card {
      background: var(--card);
      border: 1px solid var(--border);
      border-radius: 12px;
      padding: 1.25rem 1.4rem;
      margin-bottom: 1.25rem;
    }
    pre {
      margin: 0.5rem 0 0.9rem;
      padding: 0.85rem 1rem;
      background: var(--code-bg);
      border: 1px solid var(--border);
      border-radius: 8px;
      overflow-x: auto;
      font-size: 0.87rem;
      line-height: 1.45;
    }
    code {
      font-family: ui-monospace, SFMono-Regular, Menlo, Consolas, monospace;
    }
    ul {
      margin: 0.4rem 0 0;
      padding-left: 1.25rem;
      color: var(--muted);
      font-size: 0.9rem;
    }
    li { margin: 0.2rem 0; }
  </style>
</head>
<body>
  <main>
    <h1><span>OxiRoute</span> Linux Package Repositories</h1>
    <p class="meta">
      Profile-guided release packages for Debian, Ubuntu, Fedora, RHEL/AlmaLinux/Rocky Linux, Amazon Linux 2023, and openSUSE (<code>x86_64</code> &amp; <code>aarch64</code>).
      Latest indexed release: <strong>${latest_tag}</strong> &middot; Updated ${updated_utc} &middot;
      <a href="../">Site</a> &middot;
      <a href="https://github.com/${gh_repo}">GitHub</a> &middot;
      <a href="https://github.com/${gh_repo}/releases">Releases</a>
    </p>

    <section class="card">
      <h2>Debian / Ubuntu (APT — amd64 &amp; arm64)</h2>
      <p>Packages create the <code>oxiroute</code> user, state directory, and <code>/etc/oxiroute/management.token</code> without starting the service until you opt in:</p>
      <pre><code>echo "deb [trusted=yes] ${base_url}/apt ./" | sudo tee /etc/apt/sources.list.d/oxiroute.list
sudo apt update &amp;&amp; sudo apt install oxiroute</code></pre>
      <h3>Published .deb Packages</h3>
      <ul>
${deb_list}      </ul>
    </section>

    <section class="card">
      <h2>Fedora / RHEL / AlmaLinux / Rocky Linux / openSUSE (RPM — x86_64 &amp; aarch64)</h2>
      <pre><code># Fedora / RHEL / AlmaLinux / Rocky Linux
curl -fsSL ${base_url}/rpm/oxiroute.repo | sudo tee /etc/yum.repos.d/oxiroute.repo
sudo dnf install oxiroute

# openSUSE
sudo zypper addrepo --no-gpgcheck ${base_url}/rpm oxiroute
sudo zypper refresh &amp;&amp; sudo zypper install --allow-unsigned-rpm oxiroute</code></pre>
      <h3>Published el9 .rpm Packages</h3>
      <ul>
${el9_list}      </ul>

      <h3>Amazon Linux 2023</h3>
      <pre><code>curl -fsSL ${base_url}/rpm/amzn2023/oxiroute.repo | sudo tee /etc/yum.repos.d/oxiroute.repo
sudo dnf install oxiroute</code></pre>
      <ul>
${amzn_list}      </ul>
    </section>

    <section class="card">
      <h2>Arch Linux / CachyOS (AUR)</h2>
      <pre><code>paru -S oxiroute-bin     # prebuilt PGO binary
paru -S oxiroute         # build from release source
paru -S oxiroute-git     # build from main</code></pre>
    </section>
  </main>
</body>
</html>
EOF

touch "${out_dir}/.nojekyll"
printf 'Built combined GitHub Pages site + APT/RPM repositories in %s (latest: %s)\n' "${out_dir}" "${latest_tag}"
