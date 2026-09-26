//! The badge printer client against a fake printer: real loopback HTTP, the
//! real endpoint shapes, and every failure mode the desk has to survive.

use rfidex_mock::printer::{Mode, Printer};
use rfidex_runtime::printer::PrinterClient;
use uuid::Uuid;

fn id(n: u128) -> Uuid {
    Uuid::from_u128(n)
}

/// A port nothing is listening on: bound, then released.
async fn closed_port() -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    drop(listener);
    format!("http://{addr}")
}

#[tokio::test]
async fn a_working_printer_is_named_and_asked_to_reprint() {
    let printer = Printer::start().await;
    let client = PrinterClient::new(printer.base()).unwrap();
    let uuid = id(7);

    assert_eq!(client.health().await.unwrap(), "Fake Printer 2000");
    client.reprint(uuid).await.unwrap();

    let requests = printer.requests();
    assert_eq!(requests[0].method, "GET");
    assert_eq!(requests[0].path, "/health");
    assert_eq!(requests[1].method, "POST");
    assert_eq!(requests[1].path, format!("/scan/{uuid}/reprint"));
    assert_eq!(requests[1].public_id, Some(uuid));
    assert!(
        requests
            .iter()
            .all(|r| !r.headers.contains_key("authorization")),
        "no key is ever sent to the printer app"
    );
    assert!(
        requests
            .iter()
            .all(|r| !r.headers.contains_key("x-rfidex-station")),
        "the printer is not the device API"
    );
}

#[tokio::test]
async fn printer_trouble_is_reported_in_plain_language() {
    let printer = Printer::start().await;
    let client = PrinterClient::new(printer.base()).unwrap();

    for mode in [Mode::ServerError, Mode::BadJson, Mode::NotOk] {
        printer.set_mode(mode.clone());
        let health = client.health().await.unwrap_err();
        assert_eq!(
            health.message, "Printer app not running on this PC",
            "{mode:?}"
        );
        let print = client.reprint(id(7)).await.unwrap_err();
        assert_eq!(
            print.message, "Badge not printed — press Reprint",
            "{mode:?}"
        );
        for message in [health.message, print.message] {
            assert!(
                !message.contains("http") && !message.contains('/') && !message.contains("500"),
                "{mode:?} must not echo server detail: {message}"
            );
        }
    }
    // The failure still went out as one request per call: nothing retries.
    assert_eq!(printer.count(), 6);
}

#[tokio::test]
async fn a_redirect_is_never_followed() {
    let skimmer = Printer::start().await;
    let printer = Printer::with_mode(Mode::Redirect(skimmer.base().to_string())).await;
    let client = PrinterClient::new(printer.base()).unwrap();

    assert!(client.health().await.is_err());
    assert!(client.reprint(id(7)).await.is_err());
    assert_eq!(printer.count(), 2, "the fake saw the calls itself");
    assert_eq!(
        skimmer.count(),
        0,
        "a printer address may not send this app somewhere else"
    );
}

#[tokio::test]
async fn a_printer_that_is_not_running_is_not_an_error_the_desk_crashes_on() {
    let client = PrinterClient::new(&closed_port().await).unwrap();
    assert_eq!(
        client.health().await.unwrap_err().message,
        "Printer app not running on this PC"
    );
    assert_eq!(
        client.reprint(id(7)).await.unwrap_err().message,
        "Badge not printed — press Reprint"
    );
}

#[tokio::test]
async fn an_invalid_printer_address_is_refused_before_any_request() {
    for url in [
        "http://192.168.1.50:8000",
        "",
        "not a url",
        "http://x.test/path",
    ] {
        let error = match PrinterClient::new(url) {
            Ok(_) => panic!("{url} must be refused"),
            Err(error) => error,
        };
        assert_eq!(error.code, "invalid_setup", "{url}");
        assert!(!error.message.contains("http://x.test"), "{url}");
    }
}

#[tokio::test]
async fn a_held_printer_still_reports_the_request_it_received() {
    let printer = Printer::with_mode(Mode::Held).await;
    let client = PrinterClient::new(printer.base()).unwrap();
    let uuid = id(11);

    let printing = tokio::spawn({
        let client = client.clone();
        async move { client.reprint(uuid).await }
    });
    // The request is observed by notification, not by waiting on a clock.
    assert_eq!(printer.wait_for_request().await, 1);
    let requests = printer.requests();
    assert_eq!(requests[0].path, format!("/scan/{uuid}/reprint"));

    printer.release();
    printing.await.unwrap().unwrap();
    assert_eq!(printer.count(), 1, "one call, one request");
}
