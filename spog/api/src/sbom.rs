use std::collections::HashMap;

use actix_web::{web, web::ServiceConfig, HttpResponse, Responder};
use http::StatusCode;
use spog_model::search::{PackageSummary, SearchResult};
use tracing::{info, trace, warn};

use crate::{search, server::SharedState};

pub(crate) fn configure() -> impl FnOnce(&mut ServiceConfig) {
    |config: &mut ServiceConfig| {
        config.service(web::resource("/api/v1/package/search").to(search));
        config.service(web::resource("/api/v1/package").to(get));
    }
}

#[derive(Debug, serde::Deserialize)]
pub struct GetParams {
    pub id: String,
}

pub async fn get(state: web::Data<SharedState>, params: web::Query<GetParams>) -> impl Responder {
    let params = params.into_inner();

    let client = reqwest::Client::new();
    let url = if let Ok(url) = state.bombastic.join("/api/v1/sbom") {
        url
    } else {
        warn!("Error constructing bombastic search URL");
        return HttpResponse::InternalServerError().finish();
    };
    match client.get(url).query(&[("id", &params.id)]).send().await {
        Ok(response) => {
            return HttpResponse::Ok()
                .status(response.status())
                .streaming(response.bytes_stream())
        }
        Err(e) => {
            warn!("Error lookup in bombastic: {:?}", e);
            HttpResponse::InternalServerError().finish()
        }
    }
}

pub async fn search(state: web::Data<SharedState>, params: web::Query<search::QueryParams>) -> HttpResponse {
    let params = params.into_inner();
    trace!("Querying SBOM using {}", params.q);
    let client = reqwest::Client::new();

    let url = if let Ok(url) = state.bombastic.join("/api/v1/sbom/search") {
        url
    } else {
        warn!("Error constructing bombastic search URL");
        return HttpResponse::InternalServerError().finish();
    };
    match client
        .get(url)
        .query(&[("q", &params.q)])
        .query(&[("offset", params.offset), ("limit", params.limit)])
        .send()
        .await
    {
        Ok(response) => {
            if response.status() == StatusCode::OK {
                match response.json::<bombastic_model::prelude::SearchResult>().await {
                    Ok(mut data) => {
                        let mut m: HashMap<String, PackageSummary> = HashMap::new();
                        for item in data.result.drain(..) {
                            if let Some(entry) = m.get_mut(&item.purl) {
                                if !entry.dependents.contains(&item.dependent) {
                                    entry.dependents.push(item.dependent);
                                }
                            } else {
                                m.insert(
                                    item.purl.clone(),
                                    PackageSummary {
                                        purl: item.purl,
                                        name: item.name,
                                        sha256: item.sha256,
                                        license: item.license,
                                        classifier: item.classifier,
                                        supplier: item.supplier,
                                        description: item.description,
                                        dependents: vec![item.dependent],
                                        vulnerabilities: Vec::new(),
                                    },
                                );
                            }
                        }

                        let mut result = SearchResult::<Vec<PackageSummary>> {
                            total: Some(m.len()),
                            result: m.values().cloned().collect(),
                        };

                        search_vulnerabilities(state, &mut result.result).await;
                        info!("Search result: {:?}", result);
                        HttpResponse::Ok().json(result)
                    }
                    Err(e) => {
                        warn!("Error deserializing bombastic result: {:?}", e);
                        HttpResponse::InternalServerError().finish()
                    }
                }
            } else {
                return HttpResponse::Ok().status(response.status()).finish();
            }
        }
        Err(e) => {
            warn!("Error searching bombastic: {:?}", e);
            HttpResponse::InternalServerError().finish()
        }
    }
}

async fn search_vulnerabilities(state: web::Data<SharedState>, packages: &mut Vec<PackageSummary>) {
    let state = &state.vex;
    let index = state.index.read().await;
    for package in packages {
        let q = format!("affected:\"{}\"", package.purl);
        if let Ok(result) = index.search(&q, 0, 1000) {
            for summary in result.0 {
                package.vulnerabilities.push(summary.cve);
            }
        }

        info!(
            "Found {} vulns related to {}",
            package.vulnerabilities.len(),
            package.purl
        );
    }
}
