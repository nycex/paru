use crate::config::Config;
use crate::devel::{filter_devel_updates, possible_devel_updates};
use crate::fmt::color_repo;
use crate::util::{input, NumberMenu};

use alpm_utils::DbListExt;
use anyhow::Result;
use aur_depends::{AurUpdates, Resolver};
use futures::try_join;

#[derive(Default, Debug)]
pub struct Upgrades {
    pub repo_keep: Vec<String>,
    pub repo_skip: Vec<String>,
    pub aur_keep: Vec<String>,
    pub aur_skip: Vec<String>,
}

pub fn repo_upgrades(config: &Config) -> Result<Vec<alpm::Package>> {
    let flags = alpm::TransFlag::NO_LOCK;
    config.alpm.trans_init(flags)?;
    config
        .alpm
        .sync_sysupgrade(config.args.count("u", "sysupgrade") > 1)?;

    let mut pkgs = config.alpm.trans_add().iter().collect::<Vec<_>>();
    let dbs = config.alpm.syncdbs();

    pkgs.sort_by(|a, b| {
        dbs.iter()
            .position(|db| db.name() == a.db().unwrap().name())
            .unwrap()
            .cmp(
                &dbs.iter()
                    .position(|db| db.name() == b.db().unwrap().name())
                    .unwrap(),
            )
            .then(a.name().cmp(b.name()))
    });
    //config.alpm.trans_release();
    Ok(pkgs)
}

fn get_version_diff(config: &Config, old: &str, new: &str) -> (String, String) {
    let mut old_iter = old.chars();
    let mut new_iter = new.chars();
    let mut old_split = old_iter.clone();
    let old_col = config.color.old_version;
    let new_col = config.color.new_version;

    while let Some(old_c) = old_iter.next() {
        let new_c = match new_iter.next() {
            Some(c) => c,
            None => break,
        };

        if old_c != new_c {
            break;
        }

        if !old_c.is_alphanumeric() {
            old_split = old_iter.clone();
        }
    }

    let common = old.len() - old_split.as_str().len();

    (
        format!("{}{}", &old[..common], old_col.paint(&old[common..])),
        format!("{}{}", &new[..common], new_col.paint(&new[common..])),
    )
}

fn print_upgrade(
    config: &Config,
    n: usize,
    n_max: usize,
    pkg: &str,
    db: &str,
    db_pkg_max: usize,
    old: &str,
    old_max: usize,
    new: &str,
) {
    let c = config.color;
    let n = format!("{:>pad$}", n, pad = n_max);
    let db_pkg = format!(
        "{}/{}{:pad$}",
        color_repo(config.color.enabled, &db),
        c.bold.paint(pkg),
        "",
        pad = db_pkg_max - (db.len() + pkg.len()) + 1
    );
    let old = format!("{:<pad$}", old, pad = old_max);
    let (old, new) = get_version_diff(config, &old, new);
    println!(
        "{} {} {} -> {}",
        c.number_menu.paint(n),
        c.bold.paint(db_pkg),
        old,
        new
    );
}

async fn get_aur_only_upgrades<'a, 'b>(
    config: &Config,
    resolver: &mut Resolver<'a, 'b>,
    print: bool,
) -> Result<AurUpdates<'a>> {
    if config.mode != "repo" {
        if print {
            let c = config.color;
            println!(
                "{} {}",
                c.action.paint("::"),
                c.bold.paint("Looking for AUR upgrades")
            );
        }
        Ok(resolver.aur_updates().await?)
    } else {
        Ok(AurUpdates::default())
    }
}

async fn get_devel_upgrades(config: &Config, print: bool) -> Result<Vec<String>> {
    if config.devel && config.mode != "repo" {
        let c = config.color;
        if print {
            println!(
                "{} {}",
                c.action.paint("::"),
                c.bold.paint("Looking for devel upgrades")
            );
        }

        possible_devel_updates(config).await
    } else {
        Ok(Vec::new())
    }
}

pub async fn aur_upgrades<'a>(
    config: &Config,
    resolver: &mut Resolver<'a, '_>,
    print: bool,
) -> Result<(AurUpdates<'a>, Vec<String>)> {
    try_join!(
        get_aur_only_upgrades(config, resolver, print),
        get_devel_upgrades(config, print)
    )
}

pub async fn get_upgrades<'a, 'b>(
    config: &Config,
    resolver: &mut Resolver<'a, 'b>,
) -> Result<Upgrades> {
    let (aur_upgrades, devel_upgrades) = aur_upgrades(config, resolver, true).await?;

    for pkg in aur_upgrades.ignored {
        eprintln!(
            "{} {}: ignoring package upgrade ({} => {})",
            config.color.warning.paint("warning:"),
            pkg.local.name(),
            pkg.local.version(),
            pkg.remote.version
        );
    }

    let mut aur_upgrades = aur_upgrades.updates;
    let mut devel_upgrades =
        filter_devel_updates(config, resolver.cache(), &devel_upgrades).await?;

    let repo_upgrades = if config.mode != "aur" && config.combined_upgrade {
        repo_upgrades(config)?
    } else {
        Vec::new()
    };

    devel_upgrades.sort();
    devel_upgrades.dedup();
    aur_upgrades.retain(|u| !devel_upgrades.contains(&u.remote.name));

    let mut repo_skip = Vec::new();
    let mut repo_keep = Vec::new();
    let mut aur_skip = Vec::new();
    let mut aur_keep = Vec::new();

    if devel_upgrades.is_empty() && aur_upgrades.is_empty() && repo_upgrades.is_empty() {
        return Ok(Upgrades::default());
    }

    if !config.upgrade_menu {
        let mut aur = aur_upgrades
            .iter()
            .map(|p| p.remote.name.clone())
            .collect::<Vec<_>>();
        aur.extend(devel_upgrades);

        let upgrades = Upgrades {
            repo_keep: repo_upgrades.iter().map(|p| p.name().to_string()).collect(),
            aur_keep: aur,
            aur_skip,
            repo_skip,
        };
        return Ok(upgrades);
    }

    let db = config.alpm.localdb();
    let n_max = repo_upgrades.len() + aur_upgrades.len() + devel_upgrades.len();
    let n_max = n_max.to_string().len();

    let db_pkg_max = repo_upgrades
        .iter()
        .map(|u| u.name().len() + u.db().unwrap().name().len())
        .chain(aur_upgrades.iter().map(|u| u.local.name().len() + 3))
        .chain(devel_upgrades.iter().map(|u| u.len() + 5))
        .max()
        .unwrap_or(0);

    let old_max = repo_upgrades
        .iter()
        .map(|p| db.pkg(p.name()).unwrap().version().as_str().len())
        .chain(aur_upgrades.iter().map(|p| p.local.version().len()))
        .chain(
            devel_upgrades
                .iter()
                .filter_map(|p| db.pkg(p.as_str()).ok())
                .map(|p| p.version().len()),
        )
        .max()
        .unwrap_or(0);

    for (n, pkg) in repo_upgrades.iter().rev().enumerate().rev() {
        let local_pkg = config.alpm.localdb().pkg(pkg.name())?;
        print_upgrade(
            config,
            n + aur_upgrades.len() + devel_upgrades.len() + 1,
            n_max,
            pkg.name(),
            pkg.db().unwrap().name(),
            db_pkg_max,
            local_pkg.version(),
            old_max,
            pkg.version(),
        );
    }

    for (n, pkg) in aur_upgrades.iter().rev().enumerate().rev() {
        print_upgrade(
            config,
            n + devel_upgrades.len() + 1,
            n_max,
            pkg.local.name(),
            "aur",
            db_pkg_max,
            pkg.local.version(),
            old_max,
            &pkg.remote.version,
        );
    }

    for (n, pkg) in devel_upgrades.iter().rev().enumerate().rev() {
        print_upgrade(
            config,
            n + 1,
            n_max,
            pkg,
            "devel",
            db_pkg_max,
            db.pkg(pkg.as_str()).unwrap().version(),
            old_max,
            "latest-commit",
        );
    }

    let input = input(config, "Packages to exclude (eg: 1 2 3, 1-3):");
    let input = input.trim();
    let number_menu = NumberMenu::new(&input);

    for (n, pkg) in repo_upgrades.iter().rev().enumerate().rev() {
        let n = n + devel_upgrades.len() + aur_upgrades.len();
        let remote = config.alpm.syncdbs().pkg(pkg.name()).unwrap();
        let db = remote.db().unwrap();
        if !number_menu.contains(n + 1, db.name()) || input.is_empty() {
            repo_keep.push(pkg.name().to_string());
        } else {
            repo_skip.push(pkg.name().to_string());
        }
    }

    for (n, pkg) in aur_upgrades.iter().rev().enumerate().rev() {
        let n = n + devel_upgrades.len();
        if !number_menu.contains(n + 1, "aur") || input.is_empty() {
            aur_keep.push(pkg.local.name().to_string());
        } else {
            aur_skip.push(pkg.local.name().to_string());
        }
    }

    for (n, pkg) in devel_upgrades.iter().rev().enumerate().rev() {
        if !number_menu.contains(n + 1, "devel") || input.is_empty() {
            aur_keep.push(pkg.to_string());
        } else {
            aur_skip.push(pkg.to_string());
        }
    }

    let upgrades = Upgrades {
        repo_keep,
        repo_skip,
        aur_keep,
        aur_skip,
    };

    Ok(upgrades)
}
