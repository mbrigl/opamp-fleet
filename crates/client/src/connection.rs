                    // A candidate OpAMP endpoint (ADR-0013) is verified by connecting to exactly it;
                    // a redirect would defeat the point, so this probe never follows one.
                    .redirect(reqwest::redirect::Policy::none())
                .header(
                    reqwest::header::CONTENT_TYPE,
                    opamp::endpoint::PROTOBUF_CONTENT_TYPE,
                )
