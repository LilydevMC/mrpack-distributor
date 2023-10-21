use std::{env, fs};
use std::path::Path;
use std::process::Command;
use anyhow::anyhow;
use chrono::Utc;
use clap::{Parser, Subcommand, command};
use serenity::model::channel::Embed;
use serenity::model::webhook::Webhook;
use crate::models::github::{CreateReleaseRequest, ReleaseResponse};
use crate::models::meta::Config;
use crate::models::modrinth::{ProjectResponse, VersionRequest, VersionStatus, VersionType};
use crate::pack::{get_output_file, get_pack_file, write_pack_file};
use crate::util::create_temp;

mod models;
mod pack;
mod util;


#[derive(Debug, Parser)]
#[command(
    name = "mrpack distributor",
    author,
    version,
    about
)]
struct CliArgs {
    #[command(subcommand)]
    commands: Commands
}

#[derive(Debug, Subcommand)]
enum Commands {
    #[command(about = "Runs configurations.")]
    Run {
        #[clap(long, short, help = "Whether or not to send Discord webhook")]
        discord: bool,
        #[clap(long, short, help = "Custom version number")]
        version: Option<String>
    }
}

#[tokio::main]
async fn main() -> Result<(), anyhow::Error> {
    match dotenvy::dotenv() {
        Ok(_) => (),
        Err(_) => ()
    };

    let args = CliArgs::parse();

    match which::which("packwiz") {
        Ok(_) => (),
        Err(err) => return Err(anyhow!("Failed to find packwiz executable: {}", err))
    }

    match args.commands {
        Commands::Run { discord, version } => {
            if !Path::new("mrpack.toml").exists() {
                return Err(anyhow!("Failed to find `mrpack.toml` file."))
            }

            let config_file = match fs::read_to_string("mrpack.toml") {
                Ok(content_string) => {
                    let parsed_config: Config = match toml::from_str(&*content_string) {
                        Ok(config) => config,
                        Err(err) => return Err(anyhow!(
                            "Failed to parse config file: {}", err
                        ))
                    };
                    parsed_config
                },
                Err(err) => return Err(anyhow!(
                    "Failed to read config file: {}", err
                ))
            };

            let mut pack_file = match get_pack_file() {
                Ok(file) => file,
                Err(err) => return Err(err)
            };

            let tmp_info = match create_temp() {
                Ok(info) => info,
                Err(err) => return Err(err)
            };

            match version {
                Some(ver) => {
                    let mut new_file_contents = pack_file.clone();
                    new_file_contents.version = ver;
                    let file_contents_string = match toml::to_string(
                        &new_file_contents
                    ) {
                        Ok(file) => file,
                        Err(err) => return Err(anyhow!(
                            "Failed to parse new pack data to toml: {}", err
                        ))
                    };

                    pack_file = new_file_contents;

                    match write_pack_file(&tmp_info.dir_path, file_contents_string) {
                        Ok(_) => (),
                        Err(err) => return Err(err)
                    }
                },
                None => ()
            }

            match Command::new("packwiz")
                .arg("mr")
                .arg("export")
                .current_dir(&tmp_info.dir_path).output() {
                Ok(_) => (),
                Err(err) => return Err(anyhow!(
                    "Failed to export with packwiz: {}", err
                ))
            }

            let output_file_info = match get_output_file(&tmp_info) {
                Ok(file_info) => file_info,
                Err(err) => return Err(err)
            };

            let loader_opt = if pack_file.versions.quilt.is_some() {
                Some("Quilt")
            } else if pack_file.versions.fabric.is_some() {
                Some("Fabric")
            } else if pack_file.versions.forge.is_some() {
                Some("Forge")
            } else if pack_file.versions.liteloader.is_some() {
                Some("LiteLoader")
            } else {
                None
            };

            let loader = match loader_opt {
                Some(loader) => loader,
                None => return Err(anyhow!("Failed to parse loader name into string"))
            };

            let version_name = config_file.version_name_format
                .replace("%project_name%", &pack_file.name)
                .replace("%project_version%", &pack_file.version)
                .replace("%mc_version%", &pack_file.versions.minecraft)
                .replace("%loader%", loader);

            let mrpack_file_contents = match fs::read(output_file_info.file_path) {
                Ok(file) => file,
                Err(err) => return Err(anyhow!(
                    "Failed to read .mrpack file: {}", err
                ))
            };

            // Changelog

            println!("Generating changelog...");

            let first_commit = match Command::new("git")
                .args(["rev-list", "--max-parents=0", "HEAD"]).output() {
                Ok(output) => match String::from_utf8(output.stdout) {
                    Ok(output_string) => output_string.replace("\n", ""),
                    Err(err) => return Err(anyhow!(
                    "Failed to parse git output: {}", err
                ))
                },
                Err(err) => return Err(anyhow!(
                    "Failed to get first commit: {}", err
                ))
            };

            let latest_release = match reqwest::Client::new()
                .get(
                    format!(
                        "https://api.github.com/repos/{}/{}/releases/latest",
                        config_file.github.repo_owner,
                        config_file.github.repo_name
                    )
                )
                .header("User-Agent", env!("CARGO_PKG_NAME"))
                .send().await {
                    Ok(res) => {
                        match res.json::<ReleaseResponse>().await {
                            Ok(json) => Some(json),
                            Err(_) => None
                        }
                    },
                    Err(_) => None
            };


            let compare_first = match latest_release {
                Some(release) => release.tag_name,
                None => first_commit
            };


            let full_changelog = format!(
                "https://github.com/{}/{}/compare/{}..HEAD",
                config_file.github.repo_owner,
                config_file.github.repo_name,
                compare_first
            );

            let changelog_markdown = format!("[Full Changelog]({})", full_changelog);

            println!("Successfully generated changelog!");

            // GitHub Release

            println!("Creating GitHub release...");

            let github_token = match env::var("GITHUB_TOKEN") {
                Ok(token) => token,
                Err(err) => return Err(anyhow!(
                    "Failed to get `GITHUB_TOKEN`: {}", err
                ))
            };

            let new_release_req_body = CreateReleaseRequest {
                tag_name: pack_file.version.clone(),
                name: Some(version_name.clone()),
                body: Some(changelog_markdown.clone())
            };

            let new_release_response = match reqwest::Client::new()
                .post(
                    format!(
                        "https://api.github.com/repos/{}/{}/releases",
                        config_file.github.repo_owner.clone(),
                        config_file.github.repo_name.clone()
                    )
                )
                .json(&new_release_req_body)
                .header("User-Agent", env!("CARGO_PKG_NAME"))
                .header("Accept", "application/vnd.github+json")
                .bearer_auth(github_token.clone())
                .send().await {
                    Ok(res) => {
                        match res.json::<ReleaseResponse>().await {
                            Ok(json) => Ok(json),
                            Err(err) => Err(err)
                        }
                    },
                Err(err) => {
                    return Err(anyhow::Error::from(err))
                }
            };

            match new_release_response.as_ref() {
                Ok(release_res) => {
                    println!("Successfully created GitHub release!");
                    println!("Uploading release asset to GitHub release...");

                    match reqwest::Client::new()
                        .post(
                            format!(
                                "https://uploads.github.com/repos/{}/{}/releases/{}/assets?name=\"{}\"",
                                config_file.github.repo_owner,
                                config_file.github.repo_name,
                                release_res.id,
                                &output_file_info.file_name
                            )
                        )
                        .header("User-Agent", env!("CARGO_PKG_NAME"))
                        .header("Accept", "application/vnd.github+json")
                        .header("Content-Type", "application/zip")
                        .bearer_auth(github_token)
                        .body(mrpack_file_contents.clone())
                        .send().await {
                            Ok(_) => println!("Successfully uploaded release asset!"),
                            Err(_) => println!("Failed to upload release asset.")
                    };

                },
                Err(err) => println!("Failed to create GitHub release: {}", err)
            }


            // Modrinth Release

            let modrinth_config = config_file.modrinth;

            println!("Uploading to Modrinth...");

            let modrinth_req = VersionRequest {
                name: version_name.clone(),
                version_number: pack_file.version,
                changelog: Some(changelog_markdown.to_owned()),
                dependencies: vec![],
                game_versions: vec![pack_file.versions.minecraft],
                version_type: VersionType::RELEASE,
                loaders: vec![loader.to_string().to_ascii_lowercase()],
                featured: false,
                requested_status: VersionStatus::LISTED,
                project_id: modrinth_config.project_id,
                file_parts: vec!["file".to_string()],
                primary_file: output_file_info.file_name.clone(),
            };

            let modrinth_token = match env::var("MODRINTH_TOKEN") {
                Ok(token) => token,
                Err(err) => return Err(anyhow!(
                    "Failed to get `MODRINTH_TOKEN`: {}", err
                ))
            };

            let file_part = match reqwest::multipart::Part::bytes(mrpack_file_contents)
                .file_name(output_file_info.file_name.clone())
                .mime_str("application/zip") {
                Ok(part) => part,
                Err(err) => return Err(anyhow!(
                    "Failed to get part from .mrpack file: {}", err
                ))
            };

            let form = reqwest::multipart::Form::new()
                .text("data", serde_json::to_string(&modrinth_req).unwrap())
                .part("file", file_part);

            let knossos_url = match modrinth_config.staging {
                Some(is_staging) => match is_staging {
                    true => "https://staging.modrinth.com",
                    false => "https://modrinth.com"
                },
                None => "https://modrinth.com"
            };

            let labrinth_url = match modrinth_config.staging {
                Some(is_staging) => match is_staging {
                    true => "https://staging-api.modrinth.com/v2",
                    false => "https://api.modrinth.com/v2"
                },
                None => "https://api.modrinth.com/v2"
            };

            let req = match reqwest::Client::new()
                .post(format!("{}/version", labrinth_url))
                .header("Authorization", &modrinth_token)
                .multipart(form)
                .send().await {
                    Ok(res) => res,
                    Err(err) => return Err(anyhow!("Error uploading version: {}", err))
            };

            if req.status().is_success() {
                println!("Successfully uploaded version to Modrinth!");
            } else {
                return Err(anyhow!(
                    "Failed to upload version to Modrinth: {}",
                    req.text().await.unwrap()
                ))
            }

            if discord {
                let discord_config = match config_file.discord {
                    Some(config) => config,
                    None => return Err(anyhow!(
                        "Failed to get Discord config"
                    ))
                };

                let modrinth_project = match reqwest::Client::new()
                    .get(format!("{}/project/{}", labrinth_url, modrinth_req.project_id))
                    .header("Authorization", modrinth_token)
                    .send().await {
                        Ok(res) => {
                            match res.json::<ProjectResponse>().await {
                                Ok(json) => json,
                                Err(err) => return Err(anyhow!(
                                    "Error parsing response from get project: {}\n\
                                    Make sure this project is not a draft!",
                                    err.to_string()
                                ))
                            }
                        },
                        Err(err) => return Err(anyhow!(
                            "Error getting project from project id: {}",
                            err
                        ))
                };

                let description = format!("\
                **New release!** {}\n\n\
                {} [GitHub](https://github.com/{}/{}/releases/latest)\n\
                {} [Modrinth]({}/modpack/{})\n\n\
                {}
                ",
                    discord_config.discord_ping_role,
                    discord_config.github_emoji_id,
                    config_file.github.repo_owner,
                    config_file.github.repo_name,
                    discord_config.modrinth_emoji_id,
                    knossos_url,
                    modrinth_project.slug,
                    changelog_markdown
                );

                let embed = Embed::fake(|e| {
                    e.title(format!("{} {}", discord_config.title_emoji, version_name))
                        .description(description)
                        .image(discord_config.embed_image_url)
                        .footer(|f| {
                            f.text(format!("{} UTC", Utc::now().format("%b, %d %Y %r")))
                        })
                });

                let http = serenity::http::Http::new("token");
                let url = match env::var("WEBHOOK_URL") {
                    Ok(url) => url,
                    Err(err) => return Err(anyhow!(
                        "Failed to get webhook url: {}", err
                    ))
                };

                let webhook = Webhook::from_url(&http, &*url).await?;

                webhook.execute(&http, true, |w| {
                    w
                        .embeds(vec![embed])
                }).await?;

            }


            util::clean_up(&tmp_info.dir_path)?
        }
    }
    Ok(())
}
