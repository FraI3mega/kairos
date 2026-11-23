use anyhow::{bail, Context, Result};

use crate::config::Config;
use crate::{accounts, db, jobs, programs, router, App};

pub const USAGE: &str = "usage: kairos-server [serve | migrate | grant <username> <user|reviewer|admin> | program create <slug> <name> | program key <slug> [--name <name>] | program revoke <key-prefix> | program list | rebuild [<username>] | delete <username>]";

pub enum Command {
    Serve,
    Migrate,
    Grant { username: String, role: String },
    ProgramCreate { slug: String, name: String },
    ProgramKey { slug: String, name: Option<String> },
    ProgramRevoke { prefix: String },
    ProgramList,
    Rebuild { username: Option<String> },
    Delete { username: String },
}

pub fn parse(args: &[String]) -> Result<Command> {
    match args.iter().map(String::as_str).collect::<Vec<_>>().as_slice() {
        [] | ["serve"] => Ok(Command::Serve),
        ["migrate"] => Ok(Command::Migrate),
        ["grant", username, role] => Ok(Command::Grant { username: username.to_string(), role: role.to_string() }),
        ["program", "create", slug, name @ ..] if !name.is_empty() => {
            Ok(Command::ProgramCreate { slug: slug.to_string(), name: name.join(" ") })
        }
        ["program", "key", slug] => Ok(Command::ProgramKey { slug: slug.to_string(), name: None }),
        ["program", "key", slug, "--name", name @ ..] if !name.is_empty() => {
            Ok(Command::ProgramKey { slug: slug.to_string(), name: Some(name.join(" ")) })
        }
        ["program", "revoke", prefix] => Ok(Command::ProgramRevoke { prefix: prefix.to_string() }),
        ["program", "list"] => Ok(Command::ProgramList),
        ["rebuild"] => Ok(Command::Rebuild { username: None }),
        ["rebuild", username] => Ok(Command::Rebuild { username: Some(username.to_string()) }),
        ["delete", username] => Ok(Command::Delete { username: username.to_string() }),
        _ => bail!("{USAGE}"),
    }
}

fn database_url() -> Result<String> {
    std::env::var("DATABASE_URL").ok().filter(|v| !v.trim().is_empty()).context("DATABASE_URL is not set")
}

async fn migrated_pool() -> Result<deadpool_postgres::Pool> {
    let pool = db::pool(&database_url()?)?;
    db::migrate(&pool).await?;
    Ok(pool)
}

pub async fn run(cmd: Command) -> Result<()> {
    match cmd {
        Command::Migrate => {
            let pool = db::pool(&database_url()?)?;
            let applied = db::migrate(&pool).await?;
            println!("applied {} migration(s)", applied.len());
        }
        Command::Grant { username, role } => {
            let pool = db::pool(&database_url()?)?;
            db::migrate(&pool).await?;
            let old = accounts::grant(&pool, None, &username, &role, "cli").await?;
            println!("{username}: {old} -> {role}");
        }
        Command::ProgramCreate { slug, name } => {
            let pool = migrated_pool().await?;
            programs::create(&pool, None, &slug, &name, "cli").await?;
            println!("created program {slug}");
        }
        Command::ProgramKey { slug, name } => {
            let pool = migrated_pool().await?;
            let (key, prefix) = programs::new_key(&pool, None, &slug, name.as_deref(), "cli").await?;
            eprintln!("key {prefix} for {slug}, shown once, store it now:");
            println!("{key}");
        }
        Command::ProgramRevoke { prefix } => {
            let pool = migrated_pool().await?;
            let (slug, full) = programs::revoke(&pool, None, &prefix, "cli").await?;
            println!("revoked {full} ({slug})");
        }
        Command::Rebuild { username } => {
            let pool = migrated_pool().await?;
            let c = pool.get().await?;
            let ids: Vec<i64> = match &username {
                Some(u) => c.query("SELECT id FROM users WHERE username = $1", &[&u.to_lowercase()]).await?,
                None => c.query("SELECT id FROM users ORDER BY id", &[]).await?,
            }
            .iter()
            .map(|r| r.get(0))
            .collect();
            drop(c);
            if ids.is_empty() {
                bail!("no such user");
            }
            for id in &ids {
                crate::credit::rebuild_user(&pool, *id).await?;
            }
            println!("rebuilt {} user(s)", ids.len());
        }
        Command::Delete { username } => {
            let pool = migrated_pool().await?;
            let row = pool.get().await?.query_opt("SELECT id FROM users WHERE username = $1", &[&username.to_lowercase()]).await?;
            let Some(row) = row else { bail!("no such user") };
            accounts::delete(&pool, row.get(0)).await?;
            println!("deleted {username}");
        }
        Command::ProgramList => {
            let pool = migrated_pool().await?;
            for p in programs::list(&pool).await? {
                println!("{}  {}  (created {})", p.slug, p.name, p.created_at);
                for k in &p.keys {
                    let state = match &k.revoked_at {
                        Some(at) => format!("revoked {at}"),
                        None => format!("last used {}", k.last_used_at.as_deref().unwrap_or("never")),
                    };
                    println!("  {}  {}  created {}  {state}", k.prefix, k.name.as_deref().unwrap_or("-"), k.created_at);
                }
            }
        }
        Command::Serve => serve().await?,
    }
    Ok(())
}

async fn serve() -> Result<()> {
    let config = Config::from_env()?;
    let pool = db::pool(&config.database_url)?;
    db::migrate(&pool).await.context("migrating the database")?;
    if config.test_mode() {
        tracing::warn!("mock auth is on: anyone can sign in as anyone");
    }
    let bind = config.bind.clone();
    let st = App::new(config, pool)?;
    jobs::spawn(st.clone());
    let listener = tokio::net::TcpListener::bind(&bind).await?;
    tracing::info!(bind, "listening");
    axum::serve(listener, router(st).into_make_service_with_connect_info::<std::net::SocketAddr>())
        .with_graceful_shutdown(async {
            let _ = tokio::signal::ctrl_c().await;
        })
        .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(s: &[&str]) -> Vec<String> {
        s.iter().map(|x| x.to_string()).collect()
    }

    #[test]
    fn commands() {
        assert!(matches!(parse(&args(&[])).unwrap(), Command::Serve));
        assert!(matches!(parse(&args(&["migrate"])).unwrap(), Command::Migrate));
        assert!(matches!(parse(&args(&["grant", "ada", "admin"])).unwrap(), Command::Grant { .. }));
        assert!(parse(&args(&["grant", "ada"])).is_err());
        assert!(matches!(parse(&args(&["delete", "ada"])).unwrap(), Command::Delete { username } if username == "ada"));
        assert!(parse(&args(&["delete"])).is_err());
        assert!(matches!(parse(&args(&["program", "create", "stardance", "Star", "Dance"])).unwrap(), Command::ProgramCreate { name, .. } if name == "Star Dance"));
        assert!(matches!(parse(&args(&["program", "key", "stardance"])).unwrap(), Command::ProgramKey { name: None, .. }));
        assert!(matches!(parse(&args(&["program", "key", "stardance", "--name", "prod"])).unwrap(), Command::ProgramKey { name: Some(n), .. } if n == "prod"));
        assert!(matches!(parse(&args(&["program", "revoke", "kp_abc"])).unwrap(), Command::ProgramRevoke { .. }));
        assert!(matches!(parse(&args(&["program", "list"])).unwrap(), Command::ProgramList));
        assert!(parse(&args(&["program", "create", "stardance"])).is_err());
    }
}
